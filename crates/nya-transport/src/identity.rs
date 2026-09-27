use std::fmt;
use std::path::Path;

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

/// SHA-256 of a DER certificate.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of_der(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hex = std::str::from_utf8(chunk).ok()?;
            out[i] = u8::from_str_radix(hex, 16).ok()?;
        }
        Some(Self(out))
    }

    /// Short human-friendly form, e.g. `3F2A-91C0-77DE-0B14`.
    pub fn short(&self) -> String {
        self.0[..8]
            .chunks(2)
            .map(|c| format!("{:02X}{:02X}", c[0], c[1]))
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short())
    }
}

/// A self-signed TLS identity (certificate + PKCS#8 key, both DER).
#[derive(Clone)]
pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let ck = rcgen::generate_simple_self_signed(vec!["nya-remote".to_string()])
            .context("generate certificate")?;
        Ok(Self {
            cert_der: ck.cert.der().to_vec(),
            key_der: ck.key_pair.serialize_der(),
        })
    }

    /// Load `identity.cert.der` / `identity.key.der` from `dir`, creating them if absent.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join("identity.cert.der");
        let key_path = dir.join("identity.key.der");
        if cert_path.exists() && key_path.exists() {
            return Ok(Self {
                cert_der: std::fs::read(&cert_path).context("read certificate")?,
                key_der: std::fs::read(&key_path).context("read key")?,
            });
        }
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let id = Self::generate()?;
        std::fs::write(&key_path, &id.key_der).context("write key")?;
        std::fs::write(&cert_path, &id.cert_der).context("write certificate")?;
        Ok(id)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_der(&self.cert_der)
    }

    pub fn cert_chain(&self) -> Vec<CertificateDer<'static>> {
        vec![CertificateDer::from(self.cert_der.clone())]
    }

    pub fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }
}

/// Fingerprint of the peer's certificate on an established connection.
pub fn peer_fingerprint(conn: &quinn::Connection) -> Option<Fingerprint> {
    let any = conn.peer_identity()?;
    let certs = any.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    certs.first().map(|c| Fingerprint::of_der(c.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let fp = Fingerprint::of_der(b"abc");
        assert_eq!(Fingerprint::from_hex(&fp.to_hex()), Some(fp));
        assert_eq!(fp.short().len(), 19);
    }
}
