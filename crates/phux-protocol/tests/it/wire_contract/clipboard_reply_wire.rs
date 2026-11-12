//! `CLIPBOARD_REPLY`: a client's answer to a pane's OSC 52 clipboard read.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::ResourceId;
use phux_protocol::input::paste::ClipboardData;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{FrameKind, MAX_CLIPBOARD_TEXT, TYPE_CLIPBOARD_REPLY};

use crate::common::{framed_tlv, local_id_bytes as local_terminal, tlv_field};

const fn reply(text: String) -> FrameKind {
    FrameKind::ClipboardReply {
        terminal_id: ResourceId::local(3),
        selection: b'c',
        text: ClipboardData(text),
    }
}

fn round_trip(frame: &FrameKind) -> Result<FrameKind, DecodeError> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    FrameKind::decode(&encoded).map(|(decoded, _)| decoded)
}

#[test]
fn client_originated_type_byte_behind_its_own_feature_bit() {
    assert_eq!(TYPE_CLIPBOARD_REPLY, 0x18);
    assert_eq!(
        ServerFeatureExt::ClipboardRead.wire_name(),
        "CLIPBOARD_READ"
    );
    assert_eq!(reply(String::new()).type_byte(), TYPE_CLIPBOARD_REPLY);
}

#[test]
fn text_round_trips_up_to_the_cap_and_debug_hides_it() {
    for text in ["", "secret \u{1F511}", &"x".repeat(MAX_CLIPBOARD_TEXT)] {
        let frame = reply(text.to_owned());
        assert_eq!(round_trip(&frame).unwrap(), frame);
    }
    assert!(!format!("{:?}", reply("hunter2".to_owned())).contains("hunter2"));
}

#[test]
fn oversized_or_non_utf8_text_and_missing_fields_are_refused() {
    assert_eq!(
        round_trip(&reply("x".repeat(MAX_CLIPBOARD_TEXT + 1))).unwrap_err(),
        DecodeError::LengthOverflow
    );
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_terminal(1));
    tlv_field(&mut fields, 2, b"c");
    tlv_field(&mut fields, 3, b"\xff");
    assert_eq!(
        FrameKind::decode(&framed_tlv(TYPE_CLIPBOARD_REPLY, &fields)).unwrap_err(),
        DecodeError::InvalidUtf8
    );
    let mut no_selection = Vec::new();
    tlv_field(&mut no_selection, 1, &local_terminal(1));
    tlv_field(&mut no_selection, 3, b"text");
    assert!(FrameKind::decode(&framed_tlv(TYPE_CLIPBOARD_REPLY, &no_selection)).is_err());
}
