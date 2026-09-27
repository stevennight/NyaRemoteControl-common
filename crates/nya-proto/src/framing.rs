//! Varint length-delimited Protobuf messages over any async byte stream
//! (QUIC streams, named pipes).

use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("message too large: {0} bytes")]
    TooLarge(u64),
    #[error("invalid varint")]
    BadVarint,
    #[error("decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("stream closed")]
    Closed,
}

/// Read a varint. Returns `Ok(None)` on a clean EOF before the first byte.
pub async fn read_varint<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<u64>, FramingError> {
    let mut value = 0u64;
    for i in 0..10 {
        let mut b = [0u8; 1];
        let n = r.read(&mut b).await?;
        if n == 0 {
            return if i == 0 { Ok(None) } else { Err(FramingError::Closed) };
        }
        value |= u64::from(b[0] & 0x7f) << (7 * i);
        if b[0] & 0x80 == 0 {
            return Ok(Some(value));
        }
    }
    Err(FramingError::BadVarint)
}

pub fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

pub async fn write_varint<W: AsyncWrite + Unpin>(w: &mut W, v: u64) -> Result<(), FramingError> {
    let mut buf = Vec::with_capacity(10);
    encode_varint(v, &mut buf);
    w.write_all(&buf).await?;
    Ok(())
}

/// Read one length-delimited message. `Ok(None)` means the stream ended cleanly.
pub async fn read_msg<M: Message + Default, R: AsyncRead + Unpin>(
    r: &mut R,
    max_len: usize,
) -> Result<Option<M>, FramingError> {
    let Some(len) = read_varint(r).await? else {
        return Ok(None);
    };
    if len > max_len as u64 {
        return Err(FramingError::TooLarge(len));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            FramingError::Closed
        } else {
            FramingError::Io(e)
        }
    })?;
    Ok(Some(M::decode(buf.as_slice())?))
}

/// Like [`read_msg`] but treats end-of-stream as an error.
pub async fn expect_msg<M: Message + Default, R: AsyncRead + Unpin>(
    r: &mut R,
    max_len: usize,
) -> Result<M, FramingError> {
    read_msg(r, max_len).await?.ok_or(FramingError::Closed)
}

pub fn encode_msg<M: Message>(msg: &M) -> Vec<u8> {
    msg.encode_length_delimited_to_vec()
}

pub async fn write_msg<M: Message, W: AsyncWrite + Unpin>(w: &mut W, msg: &M) -> Result<(), FramingError> {
    w.write_all(&encode_msg(msg)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb;

    #[tokio::test]
    async fn roundtrip_over_duplex() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let msg = pb::ControlMsg {
            msg: Some(pb::control_msg::Msg::Ping(pb::Ping { t_us: 77 })),
        };
        let m2 = msg.clone();
        let w = tokio::spawn(async move {
            write_msg(&mut a, &m2).await.unwrap();
            write_msg(&mut a, &m2).await.unwrap();
        });
        let r1: pb::ControlMsg = expect_msg(&mut b, 1024).await.unwrap();
        let r2: pb::ControlMsg = expect_msg(&mut b, 1024).await.unwrap();
        w.await.unwrap();
        assert_eq!(r1, msg);
        assert_eq!(r2, msg);
        let end: Option<pb::ControlMsg> = read_msg(&mut b, 1024).await.unwrap();
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn rejects_oversized() {
        let (mut a, mut b) = tokio::io::duplex(64);
        tokio::spawn(async move { write_varint(&mut a, 5000).await.unwrap() });
        let r: Result<Option<pb::Ping>, _> = read_msg(&mut b, 100).await;
        assert!(matches!(r, Err(FramingError::TooLarge(5000))));
    }

    #[test]
    fn varint_encoding() {
        let mut v = Vec::new();
        encode_varint(300, &mut v);
        assert_eq!(v, [0xac, 0x02]);
    }
}
