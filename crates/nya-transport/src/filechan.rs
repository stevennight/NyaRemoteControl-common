//! Files over TCP (FEATURE_TCP_FILES): a TLS connection beside the QUIC
//! connection carries every file transfer, so bulk data has its own
//! congestion control and windows and never competes with the video inside
//! one QUIC connection (where a copied folder once starved behind it).
//!
//! * The client connects to the host's port number over TCP — the address it
//!   reached QUIC at, so a port forward needs the same port for TCP — with
//!   TLS 1.3 and the identities QUIC uses (the host's certificate pinned, the
//!   client's presented). It sends [`HELLO`] and the token the host gave it
//!   on the control stream (`FileChannel`); the host checks token and client
//!   certificate and answers one byte.
//! * Frames, both ways: kind (u8), file id (u64 LE, the sender's numbering),
//!   payload length (u32 LE), payload. HEADER (a `FileHeader`), DATA (chunks
//!   of at most [`CHUNK`]), END (SHA-256 of the file's data), ABORT (why the
//!   sender gave up). Frames of several files may interleave.
//! * The receiver gets each file as an `AsyncRead` ([`FileReader`]). Its last
//!   chunk is held back until END has been checked: a file whose checksum
//!   does not match ends in an error, never complete.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use nya_proto::pb::FileHeader;
use prost::Message;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_rustls::TlsStream;

use crate::files::Opener;
use crate::identity::{Fingerprint, Identity};

/// What the client sends first (then the 32-byte token).
pub const HELLO: &[u8; 8] = b"NYAFILE1";
/// Largest DATA payload.
pub const CHUNK: usize = 256 * 1024;
const MAX_FRAME: usize = CHUNK + 64 * 1024;

const HEADER: u8 = 1;
const DATA: u8 = 2;
const END: u8 = 3;
const ABORT: u8 = 4;

type Tls = TlsStream<TcpStream>;

/// Called for every file the peer starts sending (from the channel's reader
/// task: spawn the work, don't do it here).
pub type OnFile = Arc<dyn Fn(FileHeader, FileReader) + Send + Sync>;

/// One end of the file channel: sends files; received ones go to `OnFile`.
#[derive(Clone)]
pub struct FileChannel {
    writer: Arc<tokio::sync::Mutex<WriteHalf<Tls>>>,
    next_id: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
}

impl FileChannel {
    fn start(stream: Tls, on_file: OnFile) -> Self {
        let (r, w) = tokio::io::split(stream);
        let alive = Arc::new(AtomicBool::new(true));
        tokio::spawn(read_loop(r, on_file, alive.clone()));
        Self { writer: Arc::new(tokio::sync::Mutex::new(w)), next_id: Arc::new(AtomicU64::new(1)), alive }
    }

    /// The TCP connection still works.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Close the connection (the session ended).
    pub async fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
        let _ = self.writer.lock().await.shutdown().await;
    }

    async fn frame(&self, kind: u8, id: u64, payload: &[u8]) -> io::Result<()> {
        let mut head = [0u8; 13];
        head[0] = kind;
        head[1..9].copy_from_slice(&id.to_le_bytes());
        head[9..13].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let mut w = self.writer.lock().await;
        let r = async {
            w.write_all(&head).await?;
            w.write_all(payload).await?;
            w.flush().await
        }
        .await;
        if r.is_err() {
            self.alive.store(false, Ordering::Relaxed);
        }
        r
    }

    /// Send `header.size` bytes read from `src`; `progress(bytes)` per chunk.
    pub async fn send_reader(&self, header: FileHeader, mut src: impl AsyncRead + Unpin, mut progress: impl FnMut(u64)) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let name = if header.path.is_empty() { header.name.clone() } else { header.path.clone() };
        let size = header.size;
        self.frame(HEADER, id, &header.encode_to_vec()).await.context("文件通道")?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut left = size;
        while left > 0 {
            let want = left.min(CHUNK as u64) as usize;
            let n = match src.read(&mut buf[..want]).await {
                Ok(0) => {
                    let why = format!("{name} 在发送过程中变小了");
                    let _ = self.frame(ABORT, id, why.as_bytes()).await;
                    bail!(why);
                }
                Ok(n) => n,
                Err(e) => {
                    let why = format!("读取 {name}：{e}");
                    let _ = self.frame(ABORT, id, why.as_bytes()).await;
                    bail!(why);
                }
            };
            hash.update(&buf[..n]);
            self.frame(DATA, id, &buf[..n]).await.context("文件通道")?;
            left -= n as u64;
            progress(n as u64);
        }
        self.frame(END, id, &hash.finalize()).await.context("文件通道")?;
        Ok(())
    }

    /// Send a file from disk (opened with `open` if given).
    pub async fn send_file(&self, header: FileHeader, path: &Path, open: Option<&Opener>, progress: impl FnMut(u64)) -> Result<()> {
        let file = match open {
            Some(open) => {
                let (open, p) = (open.clone(), path.to_owned());
                tokio::task::spawn_blocking(move || open(&p)).await?.map(tokio::fs::File::from_std)
            }
            None => tokio::fs::File::open(path).await,
        }
        .with_context(|| format!("打开 {}", path.display()))?;
        self.send_reader(header, file, progress).await
    }

    /// Send bytes from memory (a clipboard image).
    pub async fn send_bytes(&self, header: FileHeader, data: &[u8]) -> Result<()> {
        self.send_reader(header, data, |_| {}).await
    }
}

/// A received file's data, as it arrives.
pub struct FileReader {
    rx: mpsc::Receiver<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
    pos: usize,
}

impl AsyncRead for FileReader {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>, out: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        loop {
            if self.pos < self.buf.len() {
                let n = (self.buf.len() - self.pos).min(out.remaining());
                let pos = self.pos;
                out.put_slice(&self.buf[pos..pos + n]);
                self.pos += n;
                return Poll::Ready(Ok(()));
            }
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e)),
                Poll::Ready(None) => return Poll::Ready(Ok(())), // end of file
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

struct Incoming {
    /// `None` once the receiver dropped its reader (the rest is skipped).
    tx: Option<mpsc::Sender<io::Result<Vec<u8>>>>,
    hash: Sha256,
    /// The latest chunk, handed over only after the next one or a good END.
    held: Option<Vec<u8>>,
}

async fn read_loop(mut r: ReadHalf<Tls>, on_file: OnFile, alive: Arc<AtomicBool>) {
    let mut files: HashMap<u64, Incoming> = HashMap::new();
    let why = loop {
        let mut head = [0u8; 13];
        if let Err(e) = r.read_exact(&mut head).await {
            break format!("{e}");
        }
        let kind = head[0];
        let id = u64::from_le_bytes(head[1..9].try_into().unwrap());
        let len = u32::from_le_bytes(head[9..13].try_into().unwrap()) as usize;
        if len > MAX_FRAME {
            break format!("frame too large ({len})");
        }
        let mut payload = vec![0u8; len];
        if let Err(e) = r.read_exact(&mut payload).await {
            break format!("{e}");
        }
        match kind {
            HEADER => {
                let Ok(h) = FileHeader::decode(&payload[..]) else { break "bad file header".into() };
                let (tx, rx) = mpsc::channel(4);
                on_file(h, FileReader { rx, buf: Vec::new(), pos: 0 });
                files.insert(id, Incoming { tx: Some(tx), hash: Sha256::new(), held: None });
            }
            DATA => {
                if let Some(f) = files.get_mut(&id) {
                    f.hash.update(&payload);
                    if let Some(prev) = f.held.replace(payload) {
                        // Waits while the receiver is busy: the TCP connection pushes back.
                        if let Some(tx) = &f.tx {
                            if tx.send(Ok(prev)).await.is_err() {
                                f.tx = None;
                            }
                        }
                    }
                }
            }
            END => {
                if let Some(f) = files.remove(&id) {
                    if let Some(tx) = f.tx {
                        if f.hash.finalize().as_slice() == payload.as_slice() {
                            if let Some(last) = f.held {
                                let _ = tx.send(Ok(last)).await;
                            }
                        } else {
                            let _ = tx.send(Err(io::Error::new(io::ErrorKind::InvalidData, "文件校验失败（SHA-256 不一致）"))).await;
                        }
                    }
                }
            }
            ABORT => {
                if let Some(f) = files.remove(&id) {
                    if let Some(tx) = f.tx {
                        let _ = tx.send(Err(io::Error::other(String::from_utf8_lossy(&payload).into_owned()))).await;
                    }
                }
            }
            _ => {}
        }
    };
    alive.store(false, Ordering::Relaxed);
    if !files.is_empty() {
        tracing::warn!("file channel closed with {} file(s) unfinished: {why}", files.len());
    } else {
        tracing::debug!("file channel closed: {why}");
    }
    for (_, f) in files {
        if let Some(tx) = f.tx {
            let _ = tx.try_send(Err(io::Error::new(io::ErrorKind::ConnectionAborted, "文件通道断开了")));
        }
    }
}

fn tune(tcp: &TcpStream) {
    let _ = tcp.set_nodelay(true);
    let sock = socket2::SockRef::from(tcp);
    let ka = socket2::TcpKeepalive::new().with_time(Duration::from_secs(10)).with_interval(Duration::from_secs(5));
    let _ = sock.set_tcp_keepalive(&ka);
}

/// Client: open the file channel to the host at `addr` with the token from
/// its `FileChannel` message.
pub async fn connect(addr: SocketAddr, id: &Identity, host: Fingerprint, token: &[u8], on_file: OnFile) -> Result<FileChannel> {
    let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow::anyhow!("TCP {addr} 连接超时"))?
        .with_context(|| format!("TCP {addr}"))?;
    tune(&tcp);
    let cfg = crate::tls::client_crypto(id, Some(host))?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(cfg));
    let name = rustls::pki_types::ServerName::try_from("nya-remote").unwrap();
    let mut tls = tokio::time::timeout(Duration::from_secs(10), connector.connect(name, tcp))
        .await
        .map_err(|_| anyhow::anyhow!("TLS 握手超时"))?
        .context("TLS")?;
    tls.write_all(HELLO).await?;
    tls.write_all(token).await?;
    tls.flush().await?;
    let mut ok = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(10), tls.read_exact(&mut ok)).await.map_err(|_| anyhow::anyhow!("被控端没有确认"))??;
    if ok[0] != 1 {
        bail!("被控端拒绝了文件通道");
    }
    Ok(FileChannel::start(TlsStream::Client(tls), on_file))
}

/// Host: file channels the sessions are waiting for, by token.
#[derive(Default)]
pub struct Expected {
    waiting: Mutex<HashMap<Vec<u8>, (Fingerprint, oneshot::Sender<Tls>)>>,
}

/// A session's place in [`Expected`]: the token for the client, and the
/// connection once it arrives (dropping it gives the place up).
pub struct Expect {
    pub token: Vec<u8>,
    rx: oneshot::Receiver<Tls>,
    expected: Arc<Expected>,
}

impl Expect {
    /// Wait for the client's connection and start the channel.
    pub async fn accept(mut self, timeout: Duration, on_file: OnFile) -> Result<FileChannel> {
        let tls = tokio::time::timeout(timeout, &mut self.rx)
            .await
            .map_err(|_| anyhow::anyhow!("客户端没有在 {} 秒内连上文件通道（TCP）", timeout.as_secs()))?
            .map_err(|_| anyhow::anyhow!("file channel gone"))?;
        Ok(FileChannel::start(tls, on_file))
    }
}

impl Drop for Expect {
    fn drop(&mut self) {
        self.expected.waiting.lock().unwrap().remove(&self.token);
    }
}

impl Expected {
    /// A session with `client` will receive a file channel.
    pub fn expect(self: &Arc<Self>, client: Fingerprint) -> Expect {
        let token: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(token.clone(), (client, tx));
        Expect { token, rx, expected: self.clone() }
    }
}

/// Host: accept file channels on `bind` (TCP) for sessions in `expected`.
pub async fn listen(bind: SocketAddr, id: &Identity, expected: Arc<Expected>) -> Result<()> {
    let listener = bind_listener(bind)?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(crate::tls::server_crypto(id)?));
    tracing::info!("file channel: listening on TCP {bind}");
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::debug!("file channel accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let (acceptor, expected) = (acceptor.clone(), expected.clone());
        tokio::spawn(async move {
            tune(&tcp);
            let r = tokio::time::timeout(Duration::from_secs(15), async move {
                let mut tls = acceptor.accept(tcp).await.context("TLS")?;
                let mut hello = [0u8; 8 + 32];
                tls.read_exact(&mut hello).await?;
                if &hello[..8] != HELLO {
                    bail!("not a file channel");
                }
                let token = hello[8..].to_vec();
                let client = tls
                    .get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|c| c.first())
                    .map(|c| Fingerprint::of_der(c.as_ref()))
                    .context("no client certificate")?;
                let waiting = expected.waiting.lock().unwrap().remove(&token);
                match waiting {
                    Some((fp, tx)) if fp == client => {
                        tls.write_all(&[1]).await?;
                        tls.flush().await?;
                        let _ = tx.send(TlsStream::Server(tls));
                        Ok(())
                    }
                    _ => {
                        let _ = tls.write_all(&[0]).await;
                        bail!("unknown token or client")
                    }
                }
            })
            .await;
            match r {
                Ok(Ok(())) => tracing::info!("file channel from {peer}"),
                Ok(Err(e)) => tracing::warn!("file channel from {peer} refused: {e:#}"),
                Err(_) => tracing::warn!("file channel from {peer}: handshake timed out"),
            }
        });
    }
}

/// Dual-stack TCP listener (IPv6 socket that takes IPv4 too, like the UDP one).
fn bind_listener(bind: SocketAddr) -> Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::for_address(bind), Type::STREAM, Some(Protocol::TCP))?;
    if bind.is_ipv6() {
        let _ = sock.set_only_v6(false);
    }
    sock.set_nonblocking(true)?;
    sock.bind(&bind.into()).with_context(|| format!("bind TCP {bind}"))?;
    sock.listen(64)?;
    Ok(TcpListener::from_std(sock.into())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nya_proto::pb;

    fn header(name: &str, size: u64) -> FileHeader {
        FileHeader { transfer_id: 1, name: name.into(), size, purpose: pb::FilePurpose::Save as i32, index: 0, count: 1, path: String::new() }
    }

    /// Both ways over one TLS connection: files interleave, arrive whole and
    /// verified; a corrupted one ends in an error; a wrong token is refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn files_both_ways_over_tcp() {
        let (host_id, client_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let expected = Arc::new(Expected::default());
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let hid = host_id.clone();
        let exp = expected.clone();
        tokio::spawn(async move { listen(addr, &hid, exp).await.unwrap() });
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Each side collects what it receives: (name, bytes or error).
        type Got = mpsc::UnboundedReceiver<(String, std::result::Result<Vec<u8>, String>)>;
        fn collector() -> (OnFile, Got) {
            let (tx, rx) = mpsc::unbounded_channel();
            let on: OnFile = Arc::new(move |h: FileHeader, mut r: FileReader| {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut v = vec![0u8; h.size as usize];
                    let res = r.read_exact(&mut v).await.map(|_| v).map_err(|e| e.to_string());
                    let _ = tx.send((h.name, res));
                });
            });
            (on, rx)
        }
        let (host_on, mut host_got) = collector();
        let (client_on, mut client_got) = collector();

        // A wrong token is refused.
        let wrong = expected.expect(client_id.fingerprint());
        assert!(connect(addr, &client_id, host_id.fingerprint(), &[7u8; 32], client_on.clone()).await.is_err());
        drop(wrong);

        let place = expected.expect(client_id.fingerprint());
        let token = place.token.clone();
        let host_side = tokio::spawn(async move { place.accept(Duration::from_secs(5), host_on).await.unwrap() });
        let client = connect(addr, &client_id, host_id.fingerprint(), &token, client_on).await.unwrap();
        let host = host_side.await.unwrap();

        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let small = b"hello".to_vec();
        let (c2, b2, s2) = (client.clone(), big.clone(), small.clone());
        let up = tokio::spawn(async move {
            let a = c2.send_bytes(header("big", b2.len() as u64), &b2);
            let b = c2.send_bytes(header("small", s2.len() as u64), &s2);
            let (a, b) = tokio::join!(a, b);
            a.unwrap();
            b.unwrap();
        });
        host.send_bytes(header("down", 4), b"abcd").await.unwrap();
        up.await.unwrap();
        let mut got: Vec<(String, Vec<u8>)> = Vec::new();
        for _ in 0..2 {
            let (n, r) = host_got.recv().await.unwrap();
            got.push((n, r.unwrap()));
        }
        got.sort();
        assert_eq!(got, vec![("big".to_string(), big), ("small".to_string(), small)]);
        let (n, r) = client_got.recv().await.unwrap();
        assert_eq!((n.as_str(), r.unwrap()), ("down", b"abcd".to_vec()));

        // A file whose checksum does not match: an error, not a complete file.
        let id = 999;
        client.frame(HEADER, id, &header("bad", 3).encode_to_vec()).await.unwrap();
        client.frame(DATA, id, b"xyz").await.unwrap();
        client.frame(END, id, &[0u8; 32]).await.unwrap();
        let (n, r) = host_got.recv().await.unwrap();
        assert_eq!(n, "bad");
        assert!(r.unwrap_err().contains("校验失败"));

        // A file the sender gives up on.
        client.frame(HEADER, 1000, &header("cut", 10).encode_to_vec()).await.unwrap();
        client.frame(DATA, 1000, b"12345").await.unwrap();
        client.frame(ABORT, 1000, "磁盘错误".as_bytes()).await.unwrap();
        let (n, r) = host_got.recv().await.unwrap();
        assert_eq!((n.as_str(), r.unwrap_err().contains("磁盘错误")), ("cut", true));

        client.close().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!host.is_alive());
    }
}
