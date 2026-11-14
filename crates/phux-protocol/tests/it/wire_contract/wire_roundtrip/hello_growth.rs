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

#[test]
fn a_hello_without_resource_kinds_accepts_the_legacy_kinds() {
    use phux_protocol::caps::ResourceKindSet;
    use phux_protocol::ids::ResourceKind;

    let legacy = client_caps(&caps(0, 0, 0, 0, (0, 0))).resource_kinds;
    assert_eq!(legacy, ResourceKindSet::LEGACY);
    for kind in [
        ResourceKind::Terminal,
        ResourceKind::AgentSession,
        ResourceKind::AccessibilityTree,
        ResourceKind::PixelSurface,
        ResourceKind::Document,
    ] {
        assert!(legacy.contains(kind), "{kind:?}");
    }
    assert!(!legacy.contains(ResourceKind::Unknown { tag: 5 }));
}

#[test]
fn resource_kinds_round_trip_exactly_with_a_newer_clients_bits() {
    use phux_protocol::caps::{ClientCapabilities, ResourceKindSet};
    use phux_protocol::ids::ResourceKind;

    let narrow = ResourceKindSet::with(&[ResourceKind::Terminal, ResourceKind::AgentSession]);
    let frame = FrameKind::Hello {
        client_name: "narrow".to_owned(),
        protocol_major: 1,
        protocol_minor: 0,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new().with_resource_kinds(narrow),
    };
    let mut encoded = bytes::BytesMut::new();
    frame.encode(&mut encoded);
    let FrameKind::Hello { client_caps, .. } = FrameKind::decode(&encoded).expect("decodes").0
    else {
        panic!("not HELLO");
    };
    assert_eq!(client_caps.resource_kinds, narrow);
    assert!(!client_caps.resource_kinds.contains(ResourceKind::Document));

    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"newer");
    tlv_field(&mut fields, 2, &1u16.to_be_bytes());
    tlv_field(&mut fields, 3, &0u16.to_be_bytes());
    tlv_field(&mut fields, 4, &0u16.to_be_bytes());
    tlv_field(&mut fields, 5, &caps(0, 0, 0, 0, (0, 0)));
    tlv_field(&mut fields, 11, &((1u64 << 63) | 1).to_be_bytes());
    let FrameKind::Hello { client_caps, .. } = FrameKind::decode(&framed_tlv(0x01, &fields))
        .expect("decodes")
        .0
    else {
        panic!("not HELLO");
    };
    assert!(client_caps.resource_kinds.contains(ResourceKind::Terminal));
    assert!(
        !client_caps
            .resource_kinds
            .contains(ResourceKind::AgentSession)
    );
    assert_eq!(client_caps.resource_kinds.as_wire(), (1u64 << 63) | 1);
}

/// The platform a `HELLO` built from these top-level fields decodes to.
fn platform_of(frame: &[u8]) -> Option<phux_protocol::caps::ClientPlatform> {
    match FrameKind::decode(frame).expect("HELLO decodes").0 {
        FrameKind::Hello { client_caps, .. } => client_caps.platform,
        other => panic!("not HELLO: {other:?}"),
    }
}

#[test]
fn platform_round_trips_and_is_absent_from_an_older_hello() {
    use phux_protocol::caps::{ClientCapabilities, ClientPlatform};

    let encode = |platform| {
        let mut encoded = bytes::BytesMut::new();
        FrameKind::Hello {
            client_name: "p".to_owned(),
            protocol_major: 1,
            protocol_minor: 0,
            protocol_patch: 0,
            client_caps: ClientCapabilities::new().with_platform(platform),
        }
        .encode(&mut encoded);
        encoded
    };
    let ios = ClientPlatform::parse("ios");
    assert_eq!(ios.expect("valid").as_str(), "ios");
    assert_eq!(platform_of(&encode(ios)), ios);
    assert_eq!(platform_of(&encode(None)), None);
    // Unreported adds no bytes: the frame is the one older builds emit.
    assert_eq!(encode(ios).len(), encode(None).len() + 3 + "ios".len());
    // The shape every client sent before the field existed.
    assert_eq!(platform_of(&hello(&caps(0, 0, 0, 0, (0, 0)))), None);
}

#[test]
fn an_invalid_platform_is_dropped_not_refused() {
    use phux_protocol::caps::ClientPlatform;

    for bad in [
        &b""[..],
        b"MacOS",
        b"mac os",
        b"seventeen-bytes-x",
        b"\xFF\xFE",
        b"ios\n",
    ] {
        let mut fields = Vec::new();
        tlv_field(&mut fields, 1, b"newer");
        tlv_field(&mut fields, 2, &1u16.to_be_bytes());
        tlv_field(&mut fields, 3, &0u16.to_be_bytes());
        tlv_field(&mut fields, 4, &0u16.to_be_bytes());
        tlv_field(&mut fields, 5, &caps(0, 0, 0, 0, (0, 0)));
        tlv_field(&mut fields, 12, bad);
        assert_eq!(platform_of(&framed_tlv(0x01, &fields)), None, "{bad:?}");
    }
    assert!(ClientPlatform::parse("sixteen-bytes-ok").is_some());
    assert!(ClientPlatform::host().is_some());
}
