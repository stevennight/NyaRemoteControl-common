//! File / clipboard-image transfer over QUIC uni streams (stream type FILE):
//! `varint type | length-delimited FileHeader | size bytes`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use nya_proto::frame::stream_type;
use nya_proto::framing::{encode_varint, expect_msg};
use nya_proto::pb::FileHeader;
use quinn::{Connection, RecvStream};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const CHUNK: usize = 256 * 1024;
/// File streams share the link with the video (same priority, served in
/// turn). Below it they starved whenever the video filled the link: a copied
/// folder stopped part way (seen at 47.5 MB) and only crawled on.
const FILE_PRIORITY: i32 = 1;
/// Clipboard images larger than this are not transferred.
pub const MAX_IMAGE_BYTES: u64 = 64 << 20;

/// Remove directories and characters Windows doesn't allow in file names.
pub fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').to_string();
    if trimmed.is_empty() || trimmed == ".." {
        "file".into()
    } else {
        trimmed
    }
}

/// `dir/name`, or `dir/name (1).ext` … if taken.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// One item of an offer: a file or a folder, with its path relative to the
/// offer ('/' separated; the first component is the copied item's name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub abs: PathBuf,
    pub rel: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Upper bound on the entries of one offer (a copied folder tree).
pub const MAX_ITEMS: usize = 50_000;

/// Copied files and folders, with folders expanded recursively. Links and
/// unreadable entries are skipped.
pub fn expand(paths: &[PathBuf]) -> Vec<Item> {
    fn walk(abs: &Path, rel: String, out: &mut Vec<Item>) {
        if out.len() >= MAX_ITEMS {
            return;
        }
        let Ok(m) = std::fs::symlink_metadata(abs) else { return };
        if m.file_type().is_symlink() {
            return;
        }
        if m.is_dir() {
            out.push(Item { abs: abs.to_path_buf(), rel: rel.clone(), size: 0, is_dir: true });
            let Ok(rd) = std::fs::read_dir(abs) else { return };
            let mut children: Vec<_> = rd.flatten().collect();
            children.sort_by_key(|e| e.file_name());
            for c in children {
                let name = c.file_name().to_string_lossy().into_owned();
                walk(&c.path(), format!("{rel}/{name}"), out);
            }
        } else if m.is_file() {
            out.push(Item { abs: abs.to_path_buf(), rel, size: m.len(), is_dir: false });
        }
    }
    let mut out = Vec::new();
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        walk(p, name, &mut out);
    }
    out
}

/// A received relative path as a safe path below the target folder: every
/// component sanitized, no "..", no drive or root.
pub fn safe_rel_path(rel: &str) -> PathBuf {
    let mut p = PathBuf::new();
    for c in rel.split(['/', '\\']).filter(|c| !c.is_empty() && *c != "." && *c != "..") {
        p.push(sanitize_name(c));
    }
    if p.as_os_str().is_empty() {
        p.push("file");
    }
    p
}

/// The top-level items of an offer received into `root` (what goes on the
/// clipboard: the copied files and folders themselves).
pub fn top_level(root: &Path, rels: impl IntoIterator<Item = String>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for rel in rels {
        if let Some(first) = safe_rel_path(&rel).components().next() {
            let p = root.join(first.as_os_str());
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Receive the payload into `root` at the header's relative path (a paste in
/// progress; the folder is fresh, existing files are replaced).
pub async fn receive_to_tree<R: AsyncRead + Unpin>(r: &mut R, h: &FileHeader, root: &Path, mut progress: impl FnMut(u64)) -> Result<PathBuf> {
    let rel = if h.path.is_empty() { h.name.as_str() } else { h.path.as_str() };
    let final_path = root.join(safe_rel_path(rel));
    if let Some(dir) = final_path.parent() {
        tokio::fs::create_dir_all(dir).await.with_context(|| format!("创建 {}", dir.display()))?;
    }
    let part = final_path.with_extension(format!(
        "{}nyapart",
        final_path.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    let mut f = tokio::fs::File::create(&part).await.with_context(|| format!("创建 {}", part.display()))?;
    let res = async {
        let mut buf = vec![0u8; CHUNK];
        let mut left = h.size;
        while left > 0 {
            let n = AsyncReadExt::read(r, &mut buf[..(left.min(CHUNK as u64) as usize)]).await?;
            if n == 0 {
                bail!("传输中断（{} 还差 {left} 字节）", rel);
            }
            f.write_all(&buf[..n]).await?;
            left -= n as u64;
            progress(n as u64);
        }
        f.flush().await?;
        Ok(())
    }
    .await;
    drop(f);
    match res {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&final_path).await;
            tokio::fs::rename(&part, &final_path).await?;
            Ok(final_path)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

/// Delete paste caches (`root/<id>`) older than a day.
pub fn prune_cache(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(24 * 3600));
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Opens the files to send another way than as this process (the host
/// service reads them as the logged-on user).
pub type Opener = std::sync::Arc<dyn Fn(&Path) -> std::io::Result<std::fs::File> + Send + Sync>;

/// Send one file (from disk) on a new uni stream. `progress(bytes)` is called per chunk.
pub async fn send_file(conn: &Connection, header: FileHeader, path: &Path, progress: impl FnMut(u64)) -> Result<()> {
    send_file_with(conn, header, path, None, progress).await
}

/// [`send_file`], opening the file with `open` if given.
pub async fn send_file_with(conn: &Connection, header: FileHeader, path: &Path, open: Option<&Opener>, mut progress: impl FnMut(u64)) -> Result<()> {
    let opened = match open {
        Some(open) => {
            let (open, p) = (open.clone(), path.to_owned());
            tokio::task::spawn_blocking(move || open(&p)).await?.map(tokio::fs::File::from_std)
        }
        None => tokio::fs::File::open(path).await,
    };
    let mut file = opened.with_context(|| format!("打开 {}", path.display()))?;
    let mut s = conn.open_uni().await?;
    s.set_priority(FILE_PRIORITY)?; // below input and cursor
    let mut prelude = Vec::new();
    encode_varint(stream_type::FILE, &mut prelude);
    prelude.extend(nya_proto::framing::encode_msg(&header));
    s.write_all(&prelude).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut left = header.size;
    while left > 0 {
        let n = file.read(&mut buf[..(left.min(CHUNK as u64) as usize)]).await?;
        if n == 0 {
            bail!("{} 在发送过程中变小了", path.display());
        }
        s.write_all(&buf[..n]).await?;
        left -= n as u64;
        progress(n as u64);
    }
    s.finish()?;
    Ok(())
}

/// Where files go: the TCP file channel when there is one (FEATURE_TCP_FILES),
/// else FILE streams on the QUIC connection.
#[derive(Clone)]
pub struct FileLink {
    conn: Connection,
    tcp: std::sync::Arc<std::sync::Mutex<Option<crate::filechan::FileChannel>>>,
}

impl FileLink {
    pub fn new(conn: Connection) -> Self {
        Self { conn, tcp: Default::default() }
    }

    /// Use (or stop using) a TCP file channel.
    pub fn set_tcp(&self, ch: Option<crate::filechan::FileChannel>) {
        *self.tcp.lock().unwrap() = ch;
    }

    /// The working TCP file channel, if any.
    pub fn tcp(&self) -> Option<crate::filechan::FileChannel> {
        self.tcp.lock().unwrap().clone().filter(|c| c.is_alive())
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Send one file (from disk, opened with `open` if given).
    pub async fn send_file(&self, header: FileHeader, path: &Path, open: Option<&Opener>, progress: impl FnMut(u64)) -> Result<()> {
        match self.tcp() {
            Some(ch) => ch.send_file(header, path, open, progress).await,
            None => send_file_with(&self.conn, header, path, open, progress).await,
        }
    }

    /// Send bytes from memory (a clipboard image).
    pub async fn send_bytes(&self, header: FileHeader, data: &[u8]) -> Result<()> {
        match self.tcp() {
            Some(ch) => ch.send_bytes(header, data).await,
            None => send_bytes(&self.conn, header, data).await,
        }
    }
}

/// Send in-memory bytes (clipboard image).
pub async fn send_bytes(conn: &Connection, header: FileHeader, data: &[u8]) -> Result<()> {
    let mut s = conn.open_uni().await?;
    s.set_priority(FILE_PRIORITY)?;
    let mut prelude = Vec::new();
    encode_varint(stream_type::FILE, &mut prelude);
    prelude.extend(nya_proto::framing::encode_msg(&header));
    s.write_all(&prelude).await?;
    s.write_all(data).await?;
    s.finish()?;
    Ok(())
}

/// Read the header of a FILE stream (the type varint was already consumed).
pub async fn read_header(r: &mut RecvStream) -> Result<FileHeader> {
    Ok(expect_msg(r, 64 * 1024).await?)
}

/// Receive the payload into `dir`, under a unique, sanitized name. Writes to
/// a `.part` file first so half-received files are never mistaken for complete ones.
pub async fn receive_to_dir<R: AsyncRead + Unpin>(r: &mut R, h: &FileHeader, dir: &Path, mut progress: impl FnMut(u64)) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await.with_context(|| format!("创建 {}", dir.display()))?;
    let name = sanitize_name(&h.name);
    let final_path = unique_path(dir, &name);
    let part = final_path.with_extension(format!(
        "{}nyapart",
        final_path.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    let mut f = tokio::fs::File::create(&part).await.with_context(|| format!("创建 {}", part.display()))?;
    let res = async {
        let mut buf = vec![0u8; CHUNK];
        let mut left = h.size;
        while left > 0 {
            let n = AsyncReadExt::read(r, &mut buf[..(left.min(CHUNK as u64) as usize)]).await?;
            if n == 0 {
                bail!("传输中断（{} 还差 {left} 字节）", h.name);
            }
            f.write_all(&buf[..n]).await?;
            left -= n as u64;
            progress(n as u64);
        }
        f.flush().await?;
        Ok(())
    }
    .await;
    drop(f);
    match res {
        Ok(()) => {
            tokio::fs::rename(&part, &final_path).await?;
            Ok(final_path)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

/// Receive a small payload into memory.
pub async fn receive_to_vec<R: AsyncRead + Unpin>(r: &mut R, h: &FileHeader, limit: u64) -> Result<Vec<u8>> {
    if h.size > limit {
        bail!("数据过大（{} 字节）", h.size);
    }
    let mut v = vec![0u8; h.size as usize];
    AsyncReadExt::read_exact(r, &mut v).await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_name("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_name(r"C:\x\..\evil.exe"), "evil.exe");
        assert_eq!(sanitize_name("what?.txt"), "what_.txt");
        assert_eq!(sanitize_name(".."), "file");
        assert_eq!(sanitize_name("name. "), "name");
        assert_eq!(sanitize_name(""), "file");
    }

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("nya-files-{}", nya_proto::now_us()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (1).txt"));
        std::fs::write(dir.join("a (1).txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        std::fs::write(dir.join("noext"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "noext"), dir.join("noext (1)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_paths() {
        assert_eq!(safe_rel_path("a/b/c.txt"), PathBuf::from("a").join("b").join("c.txt"));
        assert_eq!(safe_rel_path("../../evil.exe"), PathBuf::from("evil.exe"));
        assert_eq!(safe_rel_path(r"C:\Windows\x"), PathBuf::from("C_").join("Windows").join("x"));
        assert_eq!(safe_rel_path("/"), PathBuf::from("file"));
        let root = PathBuf::from(r"D:\cache\1");
        let top = top_level(&root, ["docs".into(), "docs/a.txt".into(), "b.txt".into()]);
        assert_eq!(top, vec![root.join("docs"), root.join("b.txt")]);
    }

    #[test]
    fn expands_folders() {
        let base = std::env::temp_dir().join(format!("nya-expand-{}", nya_proto::now_us()));
        std::fs::create_dir_all(base.join("dir/sub")).unwrap();
        std::fs::create_dir_all(base.join("dir/empty")).unwrap();
        std::fs::write(base.join("dir/sub/x.bin"), b"12345").unwrap();
        std::fs::write(base.join("one.txt"), b"1").unwrap();
        let items = expand(&[base.join("dir"), base.join("one.txt")]);
        let rels: Vec<(&str, bool, u64)> = items.iter().map(|i| (i.rel.as_str(), i.is_dir, i.size)).collect();
        assert_eq!(rels, vec![("dir", true, 0), ("dir/empty", true, 0), ("dir/sub", true, 0), ("dir/sub/x.bin", false, 5), ("one.txt", false, 1)]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn file_roundtrip_over_quic() {
        use crate::endpoint::{client_endpoint, connect, server_endpoint};
        use crate::Identity;
        let sid = Identity::generate().unwrap();
        let server = server_endpoint("127.0.0.1:0".parse().unwrap(), &sid).unwrap();
        let addr = server.local_addr().unwrap();
        let dir = std::env::temp_dir().join(format!("nya-recv-{}", nya_proto::now_us()));
        let dir2 = dir.clone();
        let recv = tokio::spawn(async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            let mut r = conn.accept_uni().await.unwrap();
            let t = nya_proto::framing::read_varint(&mut r).await.unwrap();
            assert_eq!(t, Some(stream_type::FILE));
            let h = read_header(&mut r).await.unwrap();
            let p = receive_to_dir(&mut r, &h, &dir2, |_| {}).await.unwrap();
            conn.close(0u32.into(), b"");
            p
        });
        let src = std::env::temp_dir().join(format!("nya-src-{}.bin", nya_proto::now_us()));
        let data: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let ep = client_endpoint(addr).unwrap();
        let conn = connect(&ep, addr, &Identity::generate().unwrap(), None).await.unwrap();
        let h = FileHeader { transfer_id: 1, name: "../x/data.bin".into(), size: data.len() as u64, purpose: 1, index: 0, count: 1, ..Default::default() };
        let mut sent = 0;
        send_file(&conn, h, &src, |n| sent += n).await.unwrap();
        assert_eq!(sent, data.len() as u64);
        let p = recv.await.unwrap();
        assert_eq!(p, dir.join("data.bin"));
        assert_eq!(std::fs::read(&p).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&src);
    }
}
