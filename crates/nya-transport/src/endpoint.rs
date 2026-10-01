use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, EndpointConfig, TransportConfig, VarInt};

use crate::identity::{Fingerprint, Identity};
use crate::tls;

/// Transport settings tuned for interactive streaming.
pub fn transport_config() -> TransportConfig {
    let mut t = TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(1)));
    t.max_idle_timeout(Some(VarInt::from_u32(8_000).into()));
    // Overlay networks (Tailscale etc.) often have an MTU of 1280.
    t.initial_mtu(1200);
    t.max_concurrent_uni_streams(VarInt::from_u32(64));
    t.max_concurrent_bidi_streams(VarInt::from_u32(8));
    // Large enough for 4K keyframes, small enough to keep queueing latency down.
    t.stream_receive_window(VarInt::from_u32(16 << 20));
    t.receive_window(VarInt::from_u32(32 << 20));
    t.send_window(4 << 20);
    // Video datagrams (FEATURE_VIDEO_DATAGRAM): a keyframe arrives as one burst.
    t.datagram_receive_buffer_size(Some(8 << 20));
    t.datagram_send_buffer_size(2 << 20);
    t
}

fn udp_socket(bind: SocketAddr) -> Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::for_address(bind), Type::DGRAM, Some(Protocol::UDP))?;
    if bind.is_ipv6() {
        // Dual stack so one socket serves IPv4 and IPv6 peers.
        let _ = sock.set_only_v6(false);
    }
    // Keyframes arrive in bursts; give the kernel room to absorb them.
    let _ = sock.set_recv_buffer_size(4 << 20);
    let _ = sock.set_send_buffer_size(4 << 20);
    sock.bind(&bind.into()).with_context(|| format!("bind UDP {bind}"))?;
    Ok(sock.into())
}

fn make_endpoint(bind: SocketAddr, server: Option<quinn::ServerConfig>) -> Result<Endpoint> {
    let runtime = quinn::default_runtime().ok_or_else(|| anyhow!("no async runtime"))?;
    Ok(Endpoint::new(EndpointConfig::default(), server, udp_socket(bind)?, runtime)?)
}

/// Listening endpoint for the host.
pub fn server_endpoint(bind: SocketAddr, id: &Identity) -> Result<Endpoint> {
    let crypto = QuicServerConfig::try_from(tls::server_crypto(id)?).context("QUIC server crypto")?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    cfg.transport_config(Arc::new(transport_config()));
    make_endpoint(bind, Some(cfg))
}

/// Client endpoint bound to an ephemeral port of the right address family.
pub fn client_endpoint(target: SocketAddr) -> Result<Endpoint> {
    let bind: SocketAddr = if target.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    make_endpoint(bind, None)
}

/// Connect to a host. `pinned` is the saved server fingerprint (None for first pairing).
pub async fn connect(
    endpoint: &Endpoint,
    addr: SocketAddr,
    id: &Identity,
    pinned: Option<Fingerprint>,
) -> Result<Connection> {
    let crypto = QuicClientConfig::try_from(tls::client_crypto(id, pinned)?).context("QUIC client crypto")?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(crypto));
    cfg.transport_config(Arc::new(transport_config()));
    let conn = endpoint
        .connect_with(cfg, addr, "nya-remote")?
        .await
        .with_context(|| format!("连接 {addr} 失败"))?;
    Ok(conn)
}

/// Resolve `host`, `host:port`, `ip`, `[v6]:port` to a socket address.
pub fn resolve(target: &str, default_port: u16) -> Result<SocketAddr> {
    use std::net::ToSocketAddrs;
    if let Ok(a) = target.parse::<SocketAddr>() {
        return Ok(a);
    }
    if let Ok(ip) = target.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    let with_port = if target.contains(':') { target.to_owned() } else { format!("{target}:{default_port}") };
    with_port
        .to_socket_addrs()
        .with_context(|| format!("解析地址 {target}"))?
        .next()
        .ok_or_else(|| anyhow!("地址 {target} 没有解析结果"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::peer_fingerprint;

    #[tokio::test]
    async fn loopback_with_pinning() {
        let server_id = Identity::generate().unwrap();
        let client_id = Identity::generate().unwrap();
        let server = server_endpoint("127.0.0.1:0".parse().unwrap(), &server_id).unwrap();
        let addr = server.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            let fp = peer_fingerprint(&conn).unwrap();
            let (mut s, _r) = conn.accept_bi().await.unwrap();
            s.write_all(b"ok").await.unwrap();
            s.finish().unwrap();
            conn.closed().await;
            fp
        });

        let ep = client_endpoint(addr).unwrap();
        let conn = connect(&ep, addr, &client_id, Some(server_id.fingerprint())).await.unwrap();
        assert_eq!(peer_fingerprint(&conn), Some(server_id.fingerprint()));
        let (mut s, mut r) = conn.open_bi().await.unwrap();
        s.write_all(b"hi").await.unwrap();
        let got = r.read_to_end(16).await.unwrap();
        assert_eq!(got, b"ok");
        conn.close(0u32.into(), b"bye");
        assert_eq!(accept.await.unwrap(), client_id.fingerprint());

        // Wrong pin must fail.
        let server2 = server_endpoint("127.0.0.1:0".parse().unwrap(), &server_id).unwrap();
        let addr2 = server2.local_addr().unwrap();
        tokio::spawn(async move {
            if let Some(i) = server2.accept().await {
                let _ = i.await;
            }
        });
        let wrong = Identity::generate().unwrap().fingerprint();
        assert!(connect(&ep, addr2, &client_id, Some(wrong)).await.is_err());
    }

    #[test]
    fn resolve_forms() {
        assert_eq!(resolve("10.0.0.2", 47100).unwrap(), "10.0.0.2:47100".parse().unwrap());
        assert_eq!(resolve("10.0.0.2:5000", 47100).unwrap(), "10.0.0.2:5000".parse().unwrap());
        assert_eq!(resolve("[::1]:5000", 47100).unwrap(), "[::1]:5000".parse().unwrap());
    }
}
