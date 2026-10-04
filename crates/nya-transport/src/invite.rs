//! Pairing links: everything a client needs to reach and pair with a host in
//! one string, shown on the host as a QR code (phones scan it) and as a link
//! to copy (pasted into a client, or clicked: `nyaremote://` is registered by
//! the Windows installer).
//!
//! `nyaremote://pair?v=1&n=<name>&a=<addr>,<addr>&fp=<sha-256 hex>&c=<code>`
//!
//! * `a` – addresses to try, all at once (local IPs, and the user's external
//!   address for port forwarding / frp), `host:port` each
//! * `fp` – the host certificate, pinned while connecting (optional: older
//!   hosts don't hand it out; the code alone still proves the host)
//! * `c` – the pairing code; the link is as secret as the code itself

use crate::identity::Fingerprint;
use crate::pairing::PairingKey;

pub const SCHEME: &str = "nyaremote";
const VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// The host's display name (may be empty).
    pub name: String,
    pub addresses: Vec<String>,
    pub fingerprint: Option<Fingerprint>,
    /// The pairing code, `XXXX-XXXX-…` (normalised).
    pub code: String,
}

impl Invite {
    pub fn to_link(&self) -> String {
        let mut s = format!("{SCHEME}://pair?v={VERSION}");
        if !self.name.is_empty() {
            s.push_str("&n=");
            s.push_str(&encode(&self.name));
        }
        s.push_str("&a=");
        s.push_str(&self.addresses.iter().map(|a| encode(a)).collect::<Vec<_>>().join(","));
        if let Some(fp) = &self.fingerprint {
            s.push_str("&fp=");
            s.push_str(&fp.to_hex());
        }
        s.push_str("&c=");
        s.push_str(&self.code.replace('-', ""));
        s
    }

    /// Find a pairing link in `text` (a pasted chat message, a scanned QR code).
    pub fn parse(text: &str) -> Option<Self> {
        let lower = text.to_ascii_lowercase();
        let start = lower.find(&format!("{SCHEME}:"))?;
        let link = text[start..].split(|c: char| c.is_whitespace() || c == '"' || c == '<' || c == '>').next()?;
        let query = link.split_once('?')?.1;
        let (mut name, mut addresses, mut fingerprint, mut code) = (String::new(), Vec::new(), None, None);
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "n" => name = decode(v)?,
                "a" => {
                    addresses = v.split(',').filter(|a| !a.is_empty()).map(decode).collect::<Option<Vec<_>>>()?;
                }
                "fp" => fingerprint = Some(Fingerprint::from_hex(v)?),
                "c" => code = Some(PairingKey::from_code(&decode(v)?)?.to_code()),
                _ => {} // newer fields
            }
        }
        let addresses: Vec<String> = addresses.into_iter().map(|a| a.trim().to_owned()).filter(|a| !a.is_empty()).collect();
        if addresses.is_empty() {
            return None;
        }
        Some(Self { name: name.trim().to_owned(), addresses, fingerprint, code: code? })
    }

    /// Does `text` look like a pairing link (not a host address or name)?
    pub fn looks_like(text: &str) -> bool {
        text.trim_start().to_ascii_lowercase().starts_with(&format!("{SCHEME}:"))
    }
}

/// Percent-encode everything but unreserved characters (UTF-8 bytes).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':' | b'[' | b']') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Invite {
        Invite {
            name: "办公室 PC & 1".into(),
            addresses: vec!["192.168.1.20:47100".into(), "[fd00::1]:47100".into(), "frp.example.com:7000".into()],
            fingerprint: Some(Fingerprint([0xab; 32])),
            code: PairingKey([7; 15]).to_code(),
        }
    }

    #[test]
    fn round_trip() {
        let i = sample();
        let link = i.to_link();
        assert!(link.starts_with("nyaremote://pair?v=1&n="), "{link}");
        assert!(!link.contains(' ') && link.matches('&').count() == 4, "{link}");
        assert_eq!(Invite::parse(&link), Some(i));
    }

    #[test]
    fn found_in_text_and_tolerant() {
        let i = sample();
        let text = format!("配对链接：{}\n别发给别人", i.to_link().replace("nyaremote", "NyaRemote"));
        assert_eq!(Invite::parse(&text), Some(i.clone()));
        // Without a fingerprint, code with dashes, unknown fields.
        let link = format!("nyaremote://pair?v=2&x=1&a=10.0.0.2&c={}", i.code.to_lowercase());
        let p = Invite::parse(&link).unwrap();
        assert_eq!(p.addresses, ["10.0.0.2"]);
        assert_eq!(p.fingerprint, None);
        assert_eq!(p.code, i.code);
        assert!(Invite::looks_like("  nyaremote://pair?"));
        assert!(!Invite::looks_like("192.168.1.2"));
    }

    #[test]
    fn rejects_broken_links() {
        let code = sample().code;
        assert_eq!(Invite::parse("nyaremote://pair?v=1&a=1.2.3.4"), None); // no code
        assert_eq!(Invite::parse(&format!("nyaremote://pair?c={code}")), None); // no address
        assert_eq!(Invite::parse(&format!("nyaremote://pair?a=1.2.3.4&c={code}&fp=12")), None);
        assert_eq!(Invite::parse("https://example.com"), None);
    }
}
