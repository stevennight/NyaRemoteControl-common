//! Cross-version compatibility tests (design doc §6.4).
//!
//! `tests/compat/<version>/*.bin` hold messages encoded by released versions.
//! The current code must decode them. Regenerate the files for the *current*
//! version with `NYA_BLESS=1 cargo test -p nya-proto --test compat`, and only
//! do so when cutting a release (copy `proto/nya.proto` to `proto/history/`).

use std::path::PathBuf;

use nya_proto::pb::{self, control_msg::Msg, input_msg::Ev};
use prost::Message;

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let hello = pb::Hello {
        proto_major: 1,
        proto_minor: 0,
        min_proto_major: 1,
        client_name: "pc".into(),
        client_version: "0.1.0".into(),
        features: vec![1, 2, 3],
    };
    let welcome = pb::HelloReply {
        reply: Some(pb::hello_reply::Reply::Welcome(pb::Welcome {
            proto_major: 1,
            proto_minor: 0,
            server_name: "host".into(),
            server_version: "0.1.0".into(),
            features: vec![1, 2],
            needs_pairing: true,
        })),
    };
    let start = pb::ControlMsg {
        msg: Some(Msg::StartStream(pb::StartStream {
            display_id: 1,
            config: Some(pb::StreamConfig {
                codec: pb::Codec::Hevc as i32,
                chroma: pb::Chroma::Yuv444 as i32,
                width: 2560,
                height: 1440,
                fps: 60,
                bitrate_kbps: 20000,
                mode: pb::StreamMode::Office as i32,
                ..Default::default()
            }),
            encoder_preference: "auto".into(),
            // Since 1.2; older fixtures simply lack these fields.
            display_setup: Some(pb::DisplaySetup {
                virtual_screens: vec![pb::VirtualScreen { width: 2560, height: 1440, refresh_hz: 60, scale_percent: 150 }],
                physical_off: true,
                block_local_input: true,
            }),
            slot: 1,
        })),
    };
    let key = pb::InputMsg {
        ev: Some(Ev::Key(pb::Key { scancode: 0x1d, extended: true, down: true })),
    };
    vec![
        ("hello", hello.encode_to_vec()),
        ("welcome", welcome.encode_to_vec()),
        ("start_stream", start.encode_to_vec()),
        ("input_key", key.encode_to_vec()),
    ]
}

fn compat_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("compat")
}

#[test]
fn bless_or_decode_history() {
    let current = format!("v{}.{}", nya_proto::PROTO_MAJOR, nya_proto::PROTO_MINOR);
    if std::env::var_os("NYA_BLESS").is_some() {
        let dir = compat_dir().join(&current);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in samples() {
            std::fs::write(dir.join(format!("{name}.bin")), bytes).unwrap();
        }
    }

    let mut checked = 0;
    for entry in std::fs::read_dir(compat_dir()).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let read = |n: &str| std::fs::read(dir.join(format!("{n}.bin"))).unwrap();
        let hello = pb::Hello::decode(read("hello").as_slice()).unwrap();
        assert!(hello.proto_major >= 1);
        let reply = pb::HelloReply::decode(read("welcome").as_slice()).unwrap();
        assert!(matches!(reply.reply, Some(pb::hello_reply::Reply::Welcome(_))));
        let start = pb::ControlMsg::decode(read("start_stream").as_slice()).unwrap();
        assert!(matches!(start.msg, Some(Msg::StartStream(_))));
        let key = pb::InputMsg::decode(read("input_key").as_slice()).unwrap();
        assert!(matches!(key.ev, Some(Ev::Key(_))));
        checked += 1;
    }
    assert!(checked > 0, "no compat fixtures found; run with NYA_BLESS=1 once");
}

/// Encode a field number/wire-type key plus a length-delimited payload.
fn unknown_field(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    prost::encoding::encode_key(field, prost::encoding::WireType::LengthDelimited, &mut v);
    prost::encoding::encode_varint(payload.len() as u64, &mut v);
    v.extend_from_slice(payload);
    v
}

#[test]
fn unknown_fields_are_ignored() {
    // A newer peer adds field 99 to Hello.
    let mut bytes = samples()[0].1.clone();
    bytes.extend(unknown_field(99, b"future"));
    let hello = pb::Hello::decode(bytes.as_slice()).unwrap();
    assert_eq!(hello.client_name, "pc");
}

#[test]
fn unknown_control_message_decodes_to_none() {
    // A newer peer sends ControlMsg with a oneof branch we don't know (field 500).
    let bytes = unknown_field(500, &[0x08, 0x01]);
    let msg = pb::ControlMsg::decode(bytes.as_slice()).unwrap();
    assert!(msg.msg.is_none());
}

#[test]
fn unknown_enum_value_is_preserved_as_int() {
    let cfg = pb::StreamConfig { codec: 77, ..Default::default() };
    let back = pb::StreamConfig::decode(cfg.encode_to_vec().as_slice()).unwrap();
    assert_eq!(back.codec, 77);
    assert!(pb::Codec::try_from(back.codec).is_err());
}
