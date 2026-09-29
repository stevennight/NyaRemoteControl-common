//! Version and feature negotiation (design doc §6.4).
//!
//! Each side supports a MAJOR range `[min_major, major]`. The session uses the
//! highest MAJOR both sides support. New behaviour is enabled only through the
//! intersection of feature sets, never by comparing version numbers.

use std::collections::BTreeSet;

use crate::pb::{self, Feature, RejectReason};

/// What one side of the connection supports.
#[derive(Debug, Clone)]
pub struct LocalVersion {
    pub major: u32,
    pub minor: u32,
    pub min_major: u32,
    pub features: BTreeSet<u32>,
}

impl LocalVersion {
    /// This build's version and every feature it implements.
    pub fn current() -> Self {
        Self {
            major: crate::PROTO_MAJOR,
            minor: crate::PROTO_MINOR,
            min_major: crate::MIN_PROTO_MAJOR,
            features: all_features(),
        }
    }

    pub fn has(&self, f: Feature) -> bool {
        self.features.contains(&(f as u32))
    }
}

/// Every feature implemented by this build.
pub fn all_features() -> BTreeSet<u32> {
    [
        Feature::Audio,
        Feature::LocalCursor,
        Feature::ClipboardText,
        Feature::Yuv444,
        Feature::StaticRefine,
        Feature::Sas,
        Feature::MultiGpuInfo,
        Feature::FileTransfer,
        Feature::ClipboardImage,
        Feature::Microphone,
        Feature::UsbRedirect,
        Feature::Gamepad,
        Feature::VirtualDisplay,
        Feature::ClipboardFiles,
    ]
    .into_iter()
    .map(|f| f as u32)
    .collect()
}

/// Result of a successful negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    pub major: u32,
    pub minor: u32,
    pub features: BTreeSet<u32>,
}

impl Negotiated {
    pub fn has(&self, f: Feature) -> bool {
        self.features.contains(&(f as u32))
    }
}

fn effective_min(major: u32, min_major: u32) -> u32 {
    if min_major == 0 || min_major > major {
        major
    } else {
        min_major
    }
}

/// Server side: decide the session parameters from the client's `Hello`.
pub fn negotiate(hello: &pb::Hello, server: &LocalVersion) -> Result<Negotiated, pb::Reject> {
    let c_max = hello.proto_major;
    let c_min = effective_min(hello.proto_major, hello.min_proto_major);
    let s_max = server.major;
    let s_min = effective_min(server.major, server.min_major);

    let chosen = c_max.min(s_max);
    if chosen < c_min.max(s_min) {
        let (reason, message) = if c_max < s_min {
            (
                RejectReason::ClientTooOld,
                format!(
                    "客户端协议版本过旧（客户端 {c_max}，被控端最低支持 {s_min}），请升级客户端"
                ),
            )
        } else {
            (
                RejectReason::ServerTooOld,
                format!(
                    "被控端协议版本过旧（被控端 {s_max}，客户端最低支持 {c_min}），请升级被控端"
                ),
            )
        };
        return Err(pb::Reject {
            reason: reason as i32,
            message,
            server_proto_major: s_max,
            server_min_proto_major: s_min,
        });
    }

    // MINOR is only comparable when both sides are on the same MAJOR; otherwise
    // the side that had to step down defines the behaviour of that MAJOR.
    let minor = if c_max == s_max {
        hello.proto_minor.min(server.minor)
    } else if chosen == c_max {
        hello.proto_minor
    } else {
        server.minor
    };

    let features = hello
        .features
        .iter()
        .copied()
        .filter(|f| server.features.contains(f))
        .collect();

    Ok(Negotiated { major: chosen, minor, features })
}

/// Client side: validate the server's `Welcome` against what we support.
pub fn accept_welcome(welcome: &pb::Welcome, client: &LocalVersion) -> Result<Negotiated, String> {
    let c_min = effective_min(client.major, client.min_major);
    if welcome.proto_major > client.major || welcome.proto_major < c_min {
        return Err(format!(
            "被控端选择了不支持的协议版本 {}（客户端支持 {}..={}）",
            welcome.proto_major, c_min, client.major
        ));
    }
    // Only trust features we asked for.
    let features = welcome
        .features
        .iter()
        .copied()
        .filter(|f| client.features.contains(f))
        .collect();
    Ok(Negotiated {
        major: welcome.proto_major,
        minor: welcome.proto_minor,
        features,
    })
}

/// Build the client's Hello.
pub fn hello(client: &LocalVersion, name: &str, version: &str) -> pb::Hello {
    pb::Hello {
        proto_major: client.major,
        proto_minor: client.minor,
        min_proto_major: client.min_major,
        client_name: name.to_owned(),
        client_version: version.to_owned(),
        features: client.features.iter().copied().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, min_major: u32, feats: &[u32]) -> LocalVersion {
        LocalVersion { major, minor, min_major, features: feats.iter().copied().collect() }
    }

    fn h(major: u32, minor: u32, min_major: u32, feats: &[u32]) -> pb::Hello {
        pb::Hello {
            proto_major: major,
            proto_minor: minor,
            min_proto_major: min_major,
            features: feats.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn same_version_intersects_features() {
        let n = negotiate(&h(1, 3, 1, &[1, 2, 99]), &v(1, 5, 1, &[1, 2, 3])).unwrap();
        assert_eq!(n.major, 1);
        assert_eq!(n.minor, 3);
        assert_eq!(n.features, [1, 2].into_iter().collect());
    }

    #[test]
    fn newer_client_steps_down_to_server_major() {
        let n = negotiate(&h(2, 0, 1, &[]), &v(1, 4, 1, &[])).unwrap();
        assert_eq!(n.major, 1);
        assert_eq!(n.minor, 4);
    }

    #[test]
    fn newer_server_supports_previous_major() {
        let n = negotiate(&h(1, 2, 1, &[]), &v(2, 0, 1, &[])).unwrap();
        assert_eq!(n.major, 1);
        assert_eq!(n.minor, 2);
    }

    #[test]
    fn client_too_old() {
        let r = negotiate(&h(1, 0, 1, &[]), &v(3, 0, 2, &[])).unwrap_err();
        assert_eq!(r.reason, RejectReason::ClientTooOld as i32);
        assert_eq!(r.server_min_proto_major, 2);
    }

    #[test]
    fn server_too_old() {
        let r = negotiate(&h(3, 0, 3, &[]), &v(1, 0, 1, &[])).unwrap_err();
        assert_eq!(r.reason, RejectReason::ServerTooOld as i32);
    }

    #[test]
    fn zero_min_major_means_exact() {
        let r = negotiate(&h(2, 0, 0, &[]), &v(1, 0, 1, &[])).unwrap_err();
        assert_eq!(r.reason, RejectReason::ServerTooOld as i32);
    }

    #[test]
    fn client_ignores_features_it_did_not_ask_for() {
        let w = pb::Welcome { proto_major: 1, features: vec![1, 42], ..Default::default() };
        let n = accept_welcome(&w, &v(1, 0, 1, &[1, 2])).unwrap();
        assert_eq!(n.features, [1].into_iter().collect());
    }

    #[test]
    fn client_rejects_unsupported_major() {
        let w = pb::Welcome { proto_major: 5, ..Default::default() };
        assert!(accept_welcome(&w, &v(1, 0, 1, &[])).is_err());
    }
}
