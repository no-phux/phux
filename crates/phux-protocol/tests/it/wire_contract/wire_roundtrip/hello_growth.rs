//! `HELLO` grows without a refusal: it precedes negotiation, so a value
//! this build does not know would close the connection and a newer client
//! would redial forever. Every capability byte decodes to a documented
//! default instead.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use phux_protocol::caps::{
    ColorSupport, DEFAULT_BOOTSTRAP_CHUNK_BYTES, DEFAULT_HISTORY_PAGE_BYTES,
    MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES, OutputMode, TerminalColor,
};

use super::{FrameKind, framed_tlv, tlv_field};

/// A `HELLO` at 1.0.0 carrying this positional `client_caps` record.
fn hello(caps: &[u8]) -> Vec<u8> {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"newer");
    tlv_field(&mut fields, 2, &1u16.to_be_bytes());
    tlv_field(&mut fields, 3, &0u16.to_be_bytes());
    tlv_field(&mut fields, 4, &0u16.to_be_bytes());
    tlv_field(&mut fields, 5, caps);
    framed_tlv(0x01, &fields)
}

/// A capability record with these values, then bytes a newer client added.
fn caps(color: u8, hyperlinks: u8, output: u8, palette: u8, limits: (u32, u32)) -> Vec<u8> {
    let mut caps = vec![color, 0, 0, 0, hyperlinks, output, palette];
    if palette != 0 {
        caps.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
    }
    caps.push(0x01); // profiles
    caps.extend_from_slice(&0u64.to_be_bytes()); // native codecs
    caps.extend_from_slice(&0u32.to_be_bytes()); // native features
    caps.extend_from_slice(&limits.0.to_be_bytes());
    caps.extend_from_slice(&limits.1.to_be_bytes());
    caps.extend_from_slice(&[0xEE; 4]); // a newer client's tail
    caps
}

fn client_caps(caps: &[u8]) -> phux_protocol::caps::ClientCapabilities {
    match FrameKind::decode(&hello(caps)).expect("HELLO decodes").0 {
        FrameKind::Hello { client_caps, .. } => client_caps,
        other => panic!("not HELLO: {other:?}"),
    }
}

#[test]
fn unknown_capability_values_decode_to_their_documented_defaults() {
    let decoded = client_caps(&caps(0xEE, 7, 9, 5, (u32::MAX, u32::MAX)));
    assert_eq!(decoded.color_support, ColorSupport::TrueColor);
    assert!(decoded.hyperlinks, "a nonzero flag is set");
    assert_eq!(decoded.output_mode, OutputMode::Raw);
    let colors = decoded.default_colors.expect("a nonzero presence byte");
    assert_eq!(colors.foreground, TerminalColor { r: 1, g: 2, b: 3 });
    assert_eq!(colors.background, TerminalColor { r: 4, g: 5, b: 6 });
    let limits = decoded.bootstrap.limits;
    assert_eq!(limits.max_chunk_bytes(), MAX_BOOTSTRAP_CHUNK_BYTES);
    assert_eq!(limits.max_history_page_bytes(), MAX_HISTORY_PAGE_BYTES);
}

#[test]
fn known_values_still_decode_exactly() {
    let decoded = client_caps(&caps(
        ColorSupport::Mono.as_wire(),
        0,
        OutputMode::StateSync.as_wire(),
        0,
        (4096, 8192),
    ));
    assert_eq!(decoded.color_support, ColorSupport::Mono);
    assert!(!decoded.hyperlinks);
    assert_eq!(decoded.output_mode, OutputMode::StateSync);
    assert_eq!(decoded.default_colors, None);
    assert_eq!(decoded.bootstrap.limits.max_chunk_bytes(), 4096);
    assert_eq!(decoded.bootstrap.limits.max_history_page_bytes(), 8192);
}

#[test]
fn a_zero_limit_is_unadvertised_and_takes_the_default() {
    let limits = client_caps(&caps(0, 0, 0, 0, (0, 0))).bootstrap.limits;
    assert_eq!(limits.max_chunk_bytes(), DEFAULT_BOOTSTRAP_CHUNK_BYTES);
    assert_eq!(limits.max_history_page_bytes(), DEFAULT_HISTORY_PAGE_BYTES);
}
