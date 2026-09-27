//! Pairing: a 120-bit random code shown on the host is typed into the client
//! once. Both sides then prove knowledge of the code with an HMAC over a
//! transcript that binds both certificate fingerprints and fresh nonces, so a
//! man-in-the-middle presenting a different certificate fails the check.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

use crate::identity::Fingerprint;

type HmacSha256 = Hmac<Sha256>;

pub const KEY_LEN: usize = 15;
pub const NONCE_LEN: usize = 32;

/// Crockford base32 alphabet (no I, L, O, U).
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Clone, PartialEq, Eq)]
pub struct PairingKey(pub [u8; KEY_LEN]);

impl std::fmt::Debug for PairingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingKey(..)")
    }
}

impl PairingKey {
    pub fn generate() -> Self {
        let mut k = [0u8; KEY_LEN];
        rand::thread_rng().fill_bytes(&mut k);
        Self(k)
    }

    /// `XXXX-XXXX-XXXX-XXXX-XXXX-XXXX` (24 base32 characters).
    pub fn to_code(&self) -> String {
        let mut bits: u128 = 0;
        for b in self.0 {
            bits = (bits << 8) | u128::from(b);
        }
        let mut chars = Vec::with_capacity(24);
        for i in (0..24).rev() {
            chars.push(ALPHABET[((bits >> (i * 5)) & 31) as usize] as char);
        }
        chars
            .chunks(4)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Parse a code; tolerant of case, separators and the usual look-alikes.
    pub fn from_code(code: &str) -> Option<Self> {
        let mut bits: u128 = 0;
        let mut n = 0;
        for c in code.chars() {
            let c = match c.to_ascii_uppercase() {
                '-' | ' ' | '_' => continue,
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            let v = ALPHABET.iter().position(|&a| a as char == c)? as u128;
            bits = (bits << 5) | v;
            n += 1;
        }
        if n != 24 {
            return None;
        }
        let mut k = [0u8; KEY_LEN];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (bits >> (8 * (KEY_LEN - 1 - i))) as u8;
        }
        Some(Self(k))
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.len() != KEY_LEN * 2 {
            return None;
        }
        let mut k = [0u8; KEY_LEN];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            k[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
        }
        Some(Self(k))
    }
}

pub fn nonce() -> [u8; NONCE_LEN] {
    let mut n = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut n);
    n
}

/// Values both sides must agree on.
pub struct Transcript<'a> {
    pub server_nonce: &'a [u8],
    pub client_nonce: &'a [u8],
    pub server_fp: Fingerprint,
    pub client_fp: Fingerprint,
}

impl Transcript<'_> {
    fn mac(&self, key: &PairingKey, label: &[u8]) -> HmacSha256 {
        let mut m = HmacSha256::new_from_slice(&key.0).expect("any key length");
        m.update(label);
        for part in [self.server_nonce, self.client_nonce] {
            m.update(&(part.len() as u32).to_le_bytes());
            m.update(part);
        }
        m.update(&self.server_fp.0);
        m.update(&self.client_fp.0);
        m
    }

    pub fn client_mac(&self, key: &PairingKey) -> Vec<u8> {
        self.mac(key, b"nya-pair-client-v1").finalize().into_bytes().to_vec()
    }

    pub fn server_mac(&self, key: &PairingKey) -> Vec<u8> {
        self.mac(key, b"nya-pair-server-v1").finalize().into_bytes().to_vec()
    }

    /// Constant-time check of the client's proof.
    pub fn verify_client(&self, key: &PairingKey, mac: &[u8]) -> bool {
        self.mac(key, b"nya-pair-client-v1").verify_slice(mac).is_ok()
    }

    pub fn verify_server(&self, key: &PairingKey, mac: &[u8]) -> bool {
        self.mac(key, b"nya-pair-server-v1").verify_slice(mac).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrip() {
        for _ in 0..100 {
            let k = PairingKey::generate();
            let code = k.to_code();
            assert_eq!(code.len(), 29);
            assert_eq!(PairingKey::from_code(&code), Some(k.clone()));
            assert_eq!(PairingKey::from_code(&code.to_lowercase().replace('-', " ")), Some(k.clone()));
            assert_eq!(PairingKey::from_hex(&k.to_hex()), Some(k));
        }
        assert_eq!(PairingKey::from_code("ABC"), None);
        assert_eq!(PairingKey::from_code("UUUU-UUUU-UUUU-UUUU-UUUU-UUUU"), None);
    }

    #[test]
    fn transcript_binds_fingerprints() {
        let key = PairingKey::generate();
        let sn = nonce();
        let cn = nonce();
        let t = Transcript {
            server_nonce: &sn,
            client_nonce: &cn,
            server_fp: Fingerprint::of_der(b"server"),
            client_fp: Fingerprint::of_der(b"client"),
        };
        let mac = t.client_mac(&key);
        assert!(t.verify_client(&key, &mac));
        assert!(!t.verify_server(&key, &mac));

        // A MITM with a different certificate computes a different transcript.
        let mitm = Transcript { server_fp: Fingerprint::of_der(b"mitm"), ..t };
        assert!(!mitm.verify_client(&key, &mac));
        assert!(!Transcript { server_fp: Fingerprint::of_der(b"server"), ..mitm }
            .verify_client(&PairingKey::generate(), &mac));
    }
}
