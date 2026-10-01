//! Client folders shown on the host as a drive (FEATURE_FOLDER_MOUNT).
//!
//! The host mounts a file system whose every operation becomes one FS
//! stream: it opens a bidi stream, writes an `FsRequest` and reads the
//! `FsReply` ([`call`]). The client answers from the folders the user shared
//! ([`Shares::serve`]). Requests are stateless (a path each time, no open
//! handles), so a lost connection leaves nothing behind on the client.
//!
//! Paths are '/'-separated from the drive root: "/" lists the shared folders,
//! "/<folder>/..." is inside one. Nothing outside the shared folders is
//! reachable ("..", drive letters and backslashes are refused), and folders
//! shared read-only refuse every change.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use nya_proto::frame::stream_type;
use nya_proto::framing::{encode_varint, expect_msg, write_msg};
use nya_proto::pb::{self, fs_request::Op, FsError};
use nya_proto::MAX_MESSAGE_LEN;
use quinn::{Connection, RecvStream, SendStream};

/// Largest read or write in one request.
pub const MAX_IO: usize = 512 * 1024;

/// One shared folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    /// Directory name on the host's drive.
    pub name: String,
    /// The folder on this computer.
    pub root: PathBuf,
    pub read_only: bool,
}

/// The folders this client shares, as the host sees them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shares(pub Vec<Share>);

enum Target<'a> {
    Root,
    In { share: &'a Share, path: PathBuf, top: bool },
}

fn err(e: FsError) -> pb::FsReply {
    pb::FsReply { error: e as i32, ..Default::default() }
}

fn io_error(e: &io::Error) -> FsError {
    use io::ErrorKind as K;
    match e.kind() {
        K::NotFound => FsError::FsNotFound,
        K::AlreadyExists => FsError::FsExists,
        K::PermissionDenied | K::ReadOnlyFilesystem => FsError::FsAccess,
        K::DirectoryNotEmpty => FsError::FsNotEmpty,
        K::IsADirectory => FsError::FsIsDir,
        K::NotADirectory => FsError::FsNotDir,
        K::StorageFull | K::QuotaExceeded => FsError::FsNoSpace,
        K::InvalidInput | K::InvalidFilename => FsError::FsInvalid,
        _ => match e.raw_os_error() {
            // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
            Some(5 | 32 | 33) => FsError::FsAccess,
            Some(145) => FsError::FsNotEmpty,
            Some(80 | 183) => FsError::FsExists,
            Some(2 | 3) => FsError::FsNotFound,
            Some(112) => FsError::FsNoSpace,
            _ => FsError::FsIo,
        },
    }
}

fn unix_us(t: io::Result<SystemTime>) -> i64 {
    match t {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_micros() as i64,
            Err(e) => -(e.duration().as_micros() as i64),
        },
        Err(_) => 0,
    }
}

fn from_unix_us(us: i64) -> SystemTime {
    if us >= 0 {
        UNIX_EPOCH + Duration::from_micros(us as u64)
    } else {
        UNIX_EPOCH - Duration::from_micros(us.unsigned_abs())
    }
}

fn attr(m: &fs::Metadata) -> pb::FsAttr {
    #[cfg(windows)]
    let (read_only, hidden) = {
        use std::os::windows::fs::MetadataExt;
        let a = m.file_attributes();
        (a & 0x1 != 0, a & 0x2 != 0)
    };
    #[cfg(not(windows))]
    let (read_only, hidden) = (m.permissions().readonly(), false);
    pb::FsAttr {
        dir: m.is_dir(),
        size: if m.is_dir() { 0 } else { m.len() },
        mtime_us: unix_us(m.modified()),
        atime_us: unix_us(m.accessed()),
        ctime_us: unix_us(m.created()),
        read_only,
        hidden,
    }
}

/// Open for writing; directories need backup semantics on Windows.
fn open_for_times(p: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(0x0200_0000); // FILE_FLAG_BACKUP_SEMANTICS
    }
    o.open(p)
}

#[cfg(windows)]
fn disk_space(p: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let w: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: a NUL-terminated path and three valid out pointers.
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((total, avail))
}

#[cfg(not(windows))]
fn disk_space(_: &Path) -> Option<(u64, u64)> {
    None
}

impl Shares {
    /// The list the host gets (names and read-only flags, not local paths).
    pub fn to_pb(&self) -> pb::SharedFolders {
        pb::SharedFolders {
            folders: self.0.iter().map(|s| pb::SharedFolder { name: s.name.clone(), read_only: s.read_only }).collect(),
        }
    }

    fn resolve(&self, path: &str) -> Result<Target<'_>, FsError> {
        let mut parts = path.split('/').filter(|c| !c.is_empty());
        let Some(first) = parts.next() else { return Ok(Target::Root) };
        let share = self.0.iter().find(|s| s.name.eq_ignore_ascii_case(first)).ok_or(FsError::FsNotFound)?;
        let mut p = share.root.clone();
        let mut top = true;
        for c in parts {
            if c == "." || c == ".." || c.contains(['\\', ':', '\0']) {
                return Err(FsError::FsInvalid);
            }
            p.push(c);
            top = false;
        }
        Ok(Target::In { share, path: p, top })
    }

    /// Answer one request (blocking file I/O).
    pub fn serve(&self, req: &pb::FsRequest) -> pb::FsReply {
        let Some(op) = &req.op else { return err(FsError::FsInvalid) };
        let target = match self.resolve(&req.path) {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        let (share, path, top) = match target {
            Target::Root => return self.serve_root(op),
            Target::In { share, path, top } => (share, path, top),
        };
        let writes = !matches!(op, Op::Stat(_) | Op::List(_) | Op::Read(_) | Op::Volume(_));
        // The shared folders themselves can't be removed, renamed or replaced.
        if writes && (share.read_only || top) {
            return err(FsError::FsAccess);
        }
        match self.serve_in(op, share, &path) {
            Ok(r) => r,
            Err(e) => err(io_error(&e)),
        }
    }

    fn serve_root(&self, op: &Op) -> pb::FsReply {
        let dir = pb::FsAttr { dir: true, ..Default::default() };
        match op {
            Op::Stat(_) => pb::FsReply { attr: Some(dir), ..Default::default() },
            Op::List(_) => pb::FsReply {
                entries: self
                    .0
                    .iter()
                    .map(|s| pb::FsEntry {
                        name: s.name.clone(),
                        attr: Some(fs::metadata(&s.root).map(|m| attr(&m)).unwrap_or(dir)),
                    })
                    .collect(),
                ..Default::default()
            },
            Op::Volume(_) => self.volume(self.0.first().map(|s| s.root.as_path())),
            _ => err(FsError::FsAccess),
        }
    }

    fn volume(&self, p: Option<&Path>) -> pb::FsReply {
        let (total_bytes, free_bytes) = p.and_then(disk_space).unwrap_or((0, 0));
        pb::FsReply { total_bytes, free_bytes, ..Default::default() }
    }

    fn serve_in(&self, op: &Op, share: &Share, p: &Path) -> io::Result<pb::FsReply> {
        let ok = pb::FsReply::default();
        Ok(match op {
            Op::Stat(_) => pb::FsReply { attr: Some(attr(&fs::metadata(p)?)), ..ok },
            Op::List(_) => {
                let mut entries = Vec::new();
                for e in fs::read_dir(p)? {
                    let e = e?;
                    // Skip what can't be described (vanished, no access).
                    let Ok(m) = fs::metadata(e.path()) else { continue };
                    entries.push(pb::FsEntry { name: e.file_name().to_string_lossy().into_owned(), attr: Some(attr(&m)) });
                }
                pb::FsReply { entries, ..ok }
            }
            Op::Read(r) => {
                let mut f = File::open(p)?;
                f.seek(SeekFrom::Start(r.offset))?;
                let mut data = Vec::new();
                f.take((r.len as usize).min(MAX_IO) as u64).read_to_end(&mut data)?;
                pb::FsReply { data, ..ok }
            }
            Op::Write(w) => {
                if w.data.len() > MAX_IO {
                    return Ok(err(FsError::FsInvalid));
                }
                let mut f = OpenOptions::new().write(true).open(p)?;
                f.seek(SeekFrom::Start(w.offset))?;
                f.write_all(&w.data)?;
                pb::FsReply { written: w.data.len() as u32, ..ok }
            }
            Op::Create(c) => {
                if c.dir {
                    match fs::create_dir(p) {
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && !c.exclusive && p.is_dir() => {}
                        r => r?,
                    }
                } else {
                    let mut o = OpenOptions::new();
                    o.write(true);
                    if c.exclusive {
                        o.create_new(true);
                    } else {
                        o.create(true);
                    }
                    o.open(p)?;
                }
                pb::FsReply { attr: Some(attr(&fs::metadata(p)?)), ..ok }
            }
            Op::Remove(r) => {
                if r.dir {
                    fs::remove_dir(p)?;
                } else {
                    fs::remove_file(p)?;
                }
                ok
            }
            Op::Rename(r) => {
                let to = match self.resolve(&r.to) {
                    Ok(Target::In { share: s, path, top: false }) if s == share => path,
                    // Elsewhere (another folder, the root): copy and delete instead.
                    Ok(_) => return Ok(err(FsError::FsAccess)),
                    Err(e) => return Ok(err(e)),
                };
                if !r.replace && fs::symlink_metadata(&to).is_ok() {
                    return Ok(err(FsError::FsExists));
                }
                fs::rename(p, &to)?;
                ok
            }
            Op::Truncate(t) => {
                OpenOptions::new().write(true).open(p)?.set_len(t.size)?;
                ok
            }
            Op::SetTimes(t) => {
                let mut times = fs::FileTimes::new();
                if t.mtime_us != 0 {
                    times = times.set_modified(from_unix_us(t.mtime_us));
                }
                if t.atime_us != 0 {
                    times = times.set_accessed(from_unix_us(t.atime_us));
                }
                open_for_times(p)?.set_times(times)?;
                ok
            }
            Op::Volume(_) => self.volume(Some(&share.root)),
        })
    }
}

/// Client side: answer one FS stream the host opened (its type already read).
pub async fn serve_stream(mut send: SendStream, mut recv: RecvStream, shares: std::sync::Arc<Shares>) -> Result<()> {
    let req: pb::FsRequest = expect_msg(&mut recv, MAX_MESSAGE_LEN).await?;
    let reply = tokio::task::spawn_blocking(move || shares.serve(&req)).await?;
    write_msg(&mut send, &reply).await?;
    let _ = send.finish();
    Ok(())
}

/// Host side: one request to the client.
pub async fn call(conn: &Connection, req: &pb::FsRequest) -> Result<pb::FsReply> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let mut prelude = Vec::new();
    encode_varint(stream_type::FS, &mut prelude);
    send.write_all(&prelude).await?;
    write_msg(&mut send, req).await?;
    let _ = send.finish();
    let reply: pb::FsReply = expect_msg(&mut recv, MAX_MESSAGE_LEN).await?;
    if reply.data.len() > MAX_IO {
        bail!("reply too large");
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(path: &str, op: Op) -> pb::FsRequest {
        pb::FsRequest { path: path.into(), op: Some(op) }
    }

    fn error(r: &pb::FsReply) -> FsError {
        FsError::try_from(r.error).unwrap()
    }

    fn setup() -> (PathBuf, Shares) {
        let dir = std::env::temp_dir().join(format!("nya-folders-{}", nya_proto::now_us()));
        fs::create_dir_all(dir.join("docs/sub")).unwrap();
        fs::create_dir_all(dir.join("ro")).unwrap();
        fs::write(dir.join("docs/a.txt"), b"hello world").unwrap();
        fs::write(dir.join("ro/b.txt"), b"keep").unwrap();
        fs::write(dir.join("secret.txt"), b"outside").unwrap();
        let shares = Shares(vec![
            Share { name: "文档".into(), root: dir.join("docs"), read_only: false },
            Share { name: "RO".into(), root: dir.join("ro"), read_only: true },
        ]);
        (dir, shares)
    }

    #[test]
    fn root_lists_the_shared_folders() {
        let (dir, s) = setup();
        let r = s.serve(&req("/", Op::List(pb::FsList {})));
        let names: Vec<_> = r.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["文档", "RO"]);
        assert!(r.entries.iter().all(|e| e.attr.unwrap().dir));
        assert!(s.serve(&req("/", Op::Stat(pb::FsStat {}))).attr.unwrap().dir);
        assert_eq!(error(&s.serve(&req("/x", Op::Create(pb::FsCreate::default())))), FsError::FsNotFound);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn read_write_create_rename_remove() {
        let (dir, s) = setup();
        let r = s.serve(&req("/文档/a.txt", Op::Read(pb::FsRead { offset: 6, len: 100 })));
        assert_eq!(r.data, b"world");
        let st = s.serve(&req("/文档/A.TXT", Op::Stat(pb::FsStat {}))).attr.unwrap();
        assert_eq!((st.dir, st.size), (false, 11));
        assert!(st.mtime_us > 0);

        let c = s.serve(&req("/文档/sub/new.bin", Op::Create(pb::FsCreate { dir: false, exclusive: true })));
        assert_eq!(error(&c), FsError::FsOk);
        let again = s.serve(&req("/文档/sub/new.bin", Op::Create(pb::FsCreate { dir: false, exclusive: true })));
        assert_eq!(error(&again), FsError::FsExists);
        let w = s.serve(&req("/文档/sub/new.bin", Op::Write(pb::FsWrite { offset: 3, data: b"xyz".to_vec() })));
        assert_eq!(w.written, 3);
        assert_eq!(fs::read(dir.join("docs/sub/new.bin")).unwrap(), b"\0\0\0xyz");
        s.serve(&req("/文档/sub/new.bin", Op::Truncate(pb::FsTruncate { size: 4 })));
        assert_eq!(fs::read(dir.join("docs/sub/new.bin")).unwrap(), b"\0\0\0x");
        let t = s.serve(&req("/文档/sub/new.bin", Op::SetTimes(pb::FsSetTimes { mtime_us: 1_600_000_000_000_000, atime_us: 0 })));
        assert_eq!(error(&t), FsError::FsOk);
        let st = s.serve(&req("/文档/sub/new.bin", Op::Stat(pb::FsStat {}))).attr.unwrap();
        assert_eq!(st.mtime_us, 1_600_000_000_000_000);

        let names = |p: &str| -> Vec<String> {
            let mut v: Vec<String> = s.serve(&req(p, Op::List(pb::FsList {}))).entries.into_iter().map(|e| e.name).collect();
            v.sort();
            v
        };
        assert_eq!(names("/文档"), ["a.txt", "sub"]);
        // Rename refuses to overwrite unless asked.
        let r = s.serve(&req("/文档/sub/new.bin", Op::Rename(pb::FsRename { to: "/文档/a.txt".into(), replace: false })));
        assert_eq!(error(&r), FsError::FsExists);
        let r = s.serve(&req("/文档/sub/new.bin", Op::Rename(pb::FsRename { to: "/文档/moved.bin".into(), replace: false })));
        assert_eq!(error(&r), FsError::FsOk);
        assert_eq!(names("/文档"), ["a.txt", "moved.bin", "sub"]);

        assert_eq!(error(&s.serve(&req("/文档/sub", Op::Remove(pb::FsRemove { dir: true })))), FsError::FsOk);
        assert_eq!(error(&s.serve(&req("/文档/moved.bin", Op::Remove(pb::FsRemove { dir: false })))), FsError::FsOk);
        assert_eq!(error(&s.serve(&req("/文档/nope", Op::Stat(pb::FsStat {})))), FsError::FsNotFound);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_outside_and_no_changes_to_read_only_folders() {
        let (dir, s) = setup();
        for p in ["/文档/../secret.txt", "/文档/..", "/文档/C:\\secret.txt", "/文档/sub\\..\\..\\secret.txt"] {
            let r = s.serve(&req(p, Op::Read(pb::FsRead { offset: 0, len: 10 })));
            assert_eq!(error(&r), FsError::FsInvalid, "{p}");
        }
        assert_eq!(error(&s.serve(&req("/secret.txt", Op::Stat(pb::FsStat {})))), FsError::FsNotFound);
        // Read-only: reading works, changing doesn't.
        assert_eq!(s.serve(&req("/RO/b.txt", Op::Read(pb::FsRead { offset: 0, len: 10 }))).data, b"keep");
        for op in [
            Op::Write(pb::FsWrite { offset: 0, data: b"x".to_vec() }),
            Op::Truncate(pb::FsTruncate { size: 0 }),
            Op::Remove(pb::FsRemove { dir: false }),
            Op::Rename(pb::FsRename { to: "/RO/c.txt".into(), replace: true }),
        ] {
            assert_eq!(error(&s.serve(&req("/RO/b.txt", op))), FsError::FsAccess);
        }
        assert_eq!(fs::read(dir.join("ro/b.txt")).unwrap(), b"keep");
        // The shared folders themselves stay put; renames don't leave their folder.
        assert_eq!(error(&s.serve(&req("/文档", Op::Remove(pb::FsRemove { dir: true })))), FsError::FsAccess);
        let r = s.serve(&req("/文档/a.txt", Op::Rename(pb::FsRename { to: "/RO/a.txt".into(), replace: false })));
        assert_eq!(error(&r), FsError::FsAccess);
        let r = s.serve(&req("/文档/a.txt", Op::Rename(pb::FsRename { to: "/文档/../a.txt".into(), replace: false })));
        assert_eq!(error(&r), FsError::FsInvalid);
        assert!(dir.join("docs/a.txt").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn requests_over_quic() {
        let (dir, s) = setup();
        let shares = std::sync::Arc::new(s);
        let server_id = crate::Identity::generate().unwrap();
        let client_id = crate::Identity::generate().unwrap();
        let ep = crate::endpoint::server_endpoint("127.0.0.1:0".parse().unwrap(), &server_id).unwrap();
        let addr = ep.local_addr().unwrap();
        // The "client" (file owner) accepts FS streams on its connection.
        let accept = tokio::spawn(async move {
            let host_side = ep.accept().await.unwrap().await.unwrap();
            host_side
        });
        let cep = crate::endpoint::client_endpoint(addr).unwrap();
        let client_conn = crate::endpoint::connect(&cep, addr, &client_id, Some(server_id.fingerprint())).await.unwrap();
        let host_conn = accept.await.unwrap();
        let serving = tokio::spawn({
            let shares = shares.clone();
            async move {
                while let Ok((send, mut recv)) = client_conn.accept_bi().await {
                    let ty = nya_proto::framing::read_varint(&mut recv).await.unwrap();
                    assert_eq!(ty, Some(stream_type::FS));
                    tokio::spawn(serve_stream(send, recv, shares.clone()));
                }
            }
        });
        // Several at once, like the host's file system threads.
        let mut tasks = Vec::new();
        for i in 0..20u64 {
            let c = host_conn.clone();
            tasks.push(tokio::spawn(async move {
                call(&c, &req("/文档/a.txt", Op::Read(pb::FsRead { offset: i % 5, len: 3 }))).await.unwrap()
            }));
        }
        for (i, t) in tasks.into_iter().enumerate() {
            let off = i % 5;
            assert_eq!(t.await.unwrap().data, &b"hello world"[off..off + 3]);
        }
        let big = vec![7u8; MAX_IO];
        let w = call(&host_conn, &req("/文档/a.txt", Op::Write(pb::FsWrite { offset: 0, data: big.clone() }))).await.unwrap();
        assert_eq!(w.written as usize, MAX_IO);
        let r = call(&host_conn, &req("/文档/a.txt", Op::Read(pb::FsRead { offset: 0, len: MAX_IO as u32 }))).await.unwrap();
        assert_eq!(r.data, big);
        serving.abort();
        let _ = fs::remove_dir_all(dir);
    }
}
