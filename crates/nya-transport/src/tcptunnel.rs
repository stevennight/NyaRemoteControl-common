//! QUIC over TCP: the whole session (video, audio, input, control, datagrams)
//! on a TCP connection, for networks where UDP does not get through or works
//! badly. QUIC itself is unchanged — its packets are framed on TCP (u16
//! length, packet) through a quinn socket of our own ([`TunnelSocket`]), so
//! every part of the program works the same over either.
//!
//! * The client connects to the host's port over TCP, sends [`PREAMBLE`] and
//!   then speaks QUIC as usual; its socket has the one TCP connection.
//! * The host's TCP listener (shared with the file channel, which starts with
//!   a TLS handshake instead) hands such connections to one [`TunnelSocket`]
//!   behind a second QUIC endpoint; each TCP connection is a peer address.
//! * TCP queues what it cannot send yet; QUIC would never see loss and keep
//!   growing its window, and the queue (the latency) without limit. Packets
//!   beyond [`QUEUE_LIMIT`] bytes waiting for a connection are dropped, as on
//!   UDP, so QUIC's congestion control slows down instead.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, Endpoint, EndpointConfig, UdpPoller};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::identity::Identity;

/// What a QUIC-over-TCP connection starts with (the file channel starts
/// with a TLS record, 0x16).
pub const PREAMBLE: &[u8; 8] = b"NYAQUIC1";
/// Bytes waiting for one TCP connection beyond which packets are dropped.
pub const QUEUE_LIMIT: usize = 256 * 1024;

struct Peer {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
}

/// A quinn socket whose "datagrams" travel on TCP connections.
pub struct TunnelSocket {
    local: SocketAddr,
    peers: Mutex<HashMap<SocketAddr, Peer>>,
    inbound_tx: mpsc::UnboundedSender<(SocketAddr, Vec<u8>)>,
    inbound_rx: Mutex<mpsc::UnboundedReceiver<(SocketAddr, Vec<u8>)>>,
    /// Packets dropped because a connection's queue was full.
    dropped: AtomicUsize,
}

impl fmt::Debug for TunnelSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TunnelSocket").field("local", &self.local).finish()
    }
}

impl TunnelSocket {
    pub fn new(local: SocketAddr) -> Arc<Self> {
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            local,
            peers: Mutex::new(HashMap::new()),
            inbound_tx,
            inbound_rx: Mutex::new(inbound_rx),
            dropped: AtomicUsize::new(0),
        })
    }

    /// Packets dropped so far because a TCP connection could not keep up.
    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Carry QUIC packets for peer `addr` on `tcp` (after the preamble).
    pub fn add(self: &Arc<Self>, tcp: TcpStream, addr: SocketAddr) {
        let _ = tcp.set_nodelay(true);
        let (mut r, w) = tcp.into_split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        self.peers.lock().unwrap().insert(addr, Peer { tx, queued: queued.clone() });
        tokio::spawn(async move {
            let mut w = BufWriter::with_capacity(64 * 1024, w);
            while let Some(p) = rx.recv().await {
                let mut batch = vec![p];
                while let Ok(p) = rx.try_recv() {
                    batch.push(p);
                }
                let mut ok = true;
                for p in &batch {
                    queued.fetch_sub(p.len(), Ordering::Relaxed);
                    if w.write_all(&(p.len() as u16).to_be_bytes()).await.is_err() || w.write_all(p).await.is_err() {
                        ok = false;
                        break;
                    }
                }
                if !ok || w.flush().await.is_err() {
                    break;
                }
            }
            let _ = w.shutdown().await;
        });
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let mut len = [0u8; 2];
                if r.read_exact(&mut len).await.is_err() {
                    break;
                }
                let mut p = vec![0u8; u16::from_be_bytes(len) as usize];
                if r.read_exact(&mut p).await.is_err() || me.inbound_tx.send((addr, p)).is_err() {
                    break;
                }
            }
            me.peers.lock().unwrap().remove(&addr);
        });
    }
}

#[derive(Debug)]
struct AlwaysWritable;

impl UdpPoller for AlwaysWritable {
    fn poll_writable(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncUdpSocket for TunnelSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(AlwaysWritable)
    }

    fn try_send(&self, t: &Transmit) -> io::Result<()> {
        let peers = self.peers.lock().unwrap();
        // An unknown or closed peer: the packet is lost, as on UDP.
        let Some(peer) = peers.get(&t.destination) else { return Ok(()) };
        let seg = t.segment_size.unwrap_or(t.contents.len()).max(1);
        for p in t.contents.chunks(seg) {
            if peer.queued.load(Ordering::Relaxed) + p.len() > QUEUE_LIMIT {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            peer.queued.fetch_add(p.len(), Ordering::Relaxed);
            let _ = peer.tx.send(p.to_vec());
        }
        Ok(())
    }

    fn poll_recv(&self, cx: &mut Context, bufs: &mut [IoSliceMut<'_>], meta: &mut [RecvMeta]) -> Poll<io::Result<usize>> {
        let mut rx = self.inbound_rx.lock().unwrap();
        let mut n = 0;
        while n < bufs.len().min(meta.len()) {
            let next = if n == 0 {
                match rx.poll_recv(cx) {
                    Poll::Ready(Some(x)) => x,
                    Poll::Ready(None) => return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe))),
                    Poll::Pending => return Poll::Pending,
                }
            } else {
                match rx.try_recv() {
                    Ok(x) => x,
                    Err(_) => break,
                }
            };
            let (addr, p) = next;
            let len = p.len().min(bufs[n].len());
            bufs[n][..len].copy_from_slice(&p[..len]);
            meta[n] = RecvMeta { addr, len, stride: len, ecn: None, dst_ip: None };
            n += 1;
        }
        Poll::Ready(Ok(n))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

/// Host: the QUIC endpoint for tunnelled connections (they are added to
/// `socket` by the TCP listener).
pub fn server_endpoint(socket: Arc<TunnelSocket>, id: &Identity) -> Result<Endpoint> {
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crate::tls::server_crypto(id)?).context("QUIC server crypto")?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    cfg.transport_config(Arc::new(crate::endpoint::transport_config()));
    let runtime = quinn::default_runtime().ok_or_else(|| anyhow!("no async runtime"))?;
    Ok(Endpoint::new_with_abstract_socket(EndpointConfig::default(), Some(cfg), socket, runtime)?)
}

/// Client: a QUIC endpoint whose packets to `addr` go over a new TCP
/// connection to it. Connect to `addr` with it as with a UDP endpoint.
pub async fn client_endpoint(addr: SocketAddr) -> Result<Endpoint> {
    let mut tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow!("TCP {addr} 连接超时"))?
        .with_context(|| format!("TCP {addr}"))?;
    tcp.write_all(PREAMBLE).await?;
    let local = tcp.local_addr()?;
    let socket = TunnelSocket::new(local);
    socket.add(tcp, addr);
    let runtime = quinn::default_runtime().ok_or_else(|| anyhow!("no async runtime"))?;
    Ok(Endpoint::new_with_abstract_socket(EndpointConfig::default(), None, socket, runtime)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole QUIC connection over TCP: handshake with pinning, a stream
    /// carrying a few MB, datagrams.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn quic_over_tcp() {
        let (host_id, client_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let socket = TunnelSocket::new(addr);
        let server = server_endpoint(socket.clone(), &host_id).unwrap();
        tokio::spawn(async move {
            loop {
                let (mut tcp, peer) = listener.accept().await.unwrap();
                let mut pre = [0u8; 8];
                tcp.read_exact(&mut pre).await.unwrap();
                assert_eq!(&pre, PREAMBLE);
                socket.add(tcp, peer);
            }
        });
        let host = tokio::spawn(async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            let (mut s, mut r) = conn.accept_bi().await.unwrap();
            let data = r.read_to_end(16 << 20).await.unwrap();
            s.write_all(&(data.len() as u64).to_le_bytes()).await.unwrap();
            s.finish().unwrap();
            let d = conn.read_datagram().await.unwrap();
            conn.send_datagram(d).unwrap();
            conn.closed().await;
        });

        let ep = client_endpoint(addr).await.unwrap();
        let conn = crate::endpoint::connect(&ep, addr, &client_id, Some(host_id.fingerprint())).await.unwrap();
        assert_eq!(conn.remote_address(), addr, "the host's real address (the file channel connects there)");
        let (mut s, mut r) = conn.open_bi().await.unwrap();
        let big = vec![9u8; 5 << 20];
        s.write_all(&big).await.unwrap();
        s.finish().unwrap();
        let n = r.read_to_end(8).await.unwrap();
        assert_eq!(u64::from_le_bytes(n.try_into().unwrap()), big.len() as u64);
        conn.send_datagram(b"ping"[..].into()).unwrap();
        let echo = tokio::time::timeout(Duration::from_secs(5), conn.read_datagram()).await.unwrap().unwrap();
        assert_eq!(&echo[..], b"ping");
        conn.close(0u32.into(), b"");
        let _ = tokio::time::timeout(Duration::from_secs(5), host).await;
    }
}
