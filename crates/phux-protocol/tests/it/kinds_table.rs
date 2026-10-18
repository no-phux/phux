//! The kind catalog against the decoder (ADR-0125).
//!
//! `phux_protocol::kinds` is both the discovery catalog and the source of
//! the workload-auth classifier. A sweep over every command tag and frame
//! type uses the decoder as the oracle for which are allocated, so the
//! catalog cannot miss one or invent one. Which row each message lands on is
//! pinned per variant by the unit tests in `src/kinds/samples.rs`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions fail loudly with the remedy in the message"
)]

use std::collections::BTreeSet;

use bytes::BytesMut;
use phux_protocol::ids::ResourceId;
use phux_protocol::kinds::{self, COMMAND_RULES, Carrier, Classification, FRAME_RULES, Rule};
use phux_protocol::wire::error::DecodeError;
use phux_protocol::wire::frame::{Command, FrameKind};

/// A complete `COMMAND` frame whose nested tag is `tag` and whose body is
/// otherwise empty.
fn command_frame_with_tag(tag: u8) -> Vec<u8> {
    let mut out = BytesMut::new();
    FrameKind::Command {
        request_id: 1,
        command: Command::Upgrade,
    }
    .encode(&mut out);
    let mut bytes = out.to_vec();
    *bytes.last_mut().expect("a frame has bytes") = tag;
    bytes
}

/// The decoder's answer to "is this command tag allocated?": it refuses an
/// unallocated tag as `UnknownEnumValue { field: "Command" }`, and anything
/// else (a decoded command, a truncated body) means the tag is known.
fn decoder_allocates_command_tag(tag: u8) -> bool {
    !matches!(
        FrameKind::decode(&command_frame_with_tag(tag)),
        Err(DecodeError::UnknownEnumValue {
            field: "Command",
            ..
        })
    )
}

/// The decoder's answer to "is this frame type allocated?".
fn decoder_allocates_frame_type(type_byte: u8) -> bool {
    !matches!(
        FrameKind::decode(&[0, 0, 0, 1, type_byte]),
        Err(DecodeError::UnknownFrameKind { .. })
    )
}

fn is_command_rule(rule: &Rule) -> bool {
    COMMAND_RULES.iter().any(|row| std::ptr::eq(*row, rule))
}

fn is_frame_rule(rule: &Rule) -> bool {
    FRAME_RULES.iter().any(|row| std::ptr::eq(*row, rule))
}

#[test]
fn every_command_tag_has_a_classification() {
    // The probe frame is sound: `UPGRADE` (0x0e) is bodyless, so the patched
    // byte is the nested tag.
    assert!(matches!(
        FrameKind::decode(&command_frame_with_tag(0x0e)),
        Ok((
            FrameKind::Command {
                command: Command::Upgrade,
                ..
            },
            _
        ))
    ));
    for tag in 0..=u8::MAX {
        let method = kinds::command_method(tag);
        assert_eq!(
            method.is_some(),
            decoder_allocates_command_tag(tag),
            "command tag {tag:#04x}: the catalog and the decoder disagree about \
             whether it is allocated; add or remove its MethodSpec in kinds.rs"
        );
        let Some(method) = method else {
            continue;
        };
        assert!(
            !method.rules.is_empty() && method.rules.iter().all(|rule| is_command_rule(rule)),
            "{} must name rows of COMMAND_RULES",
            method.name
        );
    }
}

#[test]
fn every_client_frame_type_has_a_classification() {
    for method in kinds::methods() {
        let Carrier::Frame(type_byte) = method.carrier else {
            continue;
        };
        assert_eq!(
            decoder_allocates_frame_type(type_byte),
            method.shipped,
            "{} ({type_byte:#04x}): a shipped frame must decode and a \
             spec-only one must not",
            method.name
        );
        assert!(
            method.rules.iter().all(|rule| is_frame_rule(rule)),
            "{} must name rows of FRAME_RULES",
            method.name
        );
    }
    // A server-to-client frame sent by a client is wrong-direction.
    assert_eq!(
        kinds::classify_frame(&FrameKind::Pong { nonce: 1 }),
        Classification::Deny
    );
    assert!(kinds::frame_method(FrameKind::Pong { nonce: 1 }.type_byte()).is_none());
    let local = ResourceId::local(1);
    assert!(
        kinds::frame_method(
            FrameKind::ResizeTerminal {
                terminal_id: local,
                cols: 80,
                rows: 24,
                cell_px: None,
            }
            .type_byte()
        )
        .is_some()
    );
}

#[test]
fn unknown_and_retired_tags_classify_as_deny() {
    // SPAWN 0x00, RESIZE_TERMINAL 0x04, RUN_HOOK 0x06 (unallocated); 0x0a and
    // 0x0b (freed by the dissolved session verbs).
    // 0x2f..=0x34 are reserved for the workspace commands (L1 §5.7).
    for tag in [
        0x00, 0x04, 0x06, 0x0a, 0x0b, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34,
    ] {
        assert!(kinds::command_method(tag).is_none(), "{tag:#04x}");
        assert!(!decoder_allocates_command_tag(tag), "{tag:#04x}");
    }
    // SUBSCRIBE (0x40) is unallocated; INPUT_RAW (0x13) is spec-only.
    assert!(kinds::frame_method(0x40).is_none());
    assert!(!decoder_allocates_frame_type(0x40));
    for case in [
        "`SPAWN` (unallocated)",
        "`RESIZE_TERMINAL` (unallocated)",
        "`RUN_HOOK` (unallocated)",
        "Unknown, retired, or otherwise unclassified command tag",
    ] {
        let rule = COMMAND_RULES
            .iter()
            .find(|rule| rule.case == case)
            .unwrap_or_else(|| panic!("no command row {case}"));
        assert_eq!(rule.classification(), Classification::Deny, "{case}");
    }
    for case in [
        "`SUBSCRIBE` (unallocated)",
        "Unknown, wrong-direction, retired, or otherwise unclassified frame",
    ] {
        let rule = FRAME_RULES
            .iter()
            .find(|rule| rule.case == case)
            .unwrap_or_else(|| panic!("no frame row {case}"));
        assert_eq!(rule.classification(), Classification::Deny, "{case}");
    }
}

/// Rows for traffic no method carries: unallocated frames and tags, and the
/// command default-deny row. Every other row must be reachable from some
/// catalog method; the frame default-deny row is, because a spawn with an
/// unclassified binding lands on it.
const UNCARRIED_ROWS: [&str; 5] = [
    "`SUBSCRIBE` (unallocated)",
    "`SPAWN` (unallocated)",
    "`RESIZE_TERMINAL` (unallocated)",
    "`RUN_HOOK` (unallocated)",
    "Unknown, retired, or otherwise unclassified command tag",
];

#[test]
fn catalog_rows_and_methods_are_consistent() {
    let methods: Vec<_> = kinds::methods().collect();
    let names: BTreeSet<_> = methods.iter().map(|method| method.name).collect();
    assert_eq!(names.len(), methods.len(), "method names are unique");
    for (i, method) in methods.iter().enumerate() {
        assert!(
            methods[..i]
                .iter()
                .all(|other| other.carrier != method.carrier),
            "{} shares a carrier with another method",
            method.name
        );
        let in_table = match method.carrier {
            Carrier::Command(_) => is_command_rule,
            Carrier::Frame(_) | Carrier::Metadata(_) => is_frame_rule,
        };
        assert!(
            method.rules.iter().all(|rule| in_table(rule)),
            "{} names a row outside its table",
            method.name
        );
    }
    for rule in FRAME_RULES.iter().chain(COMMAND_RULES.iter()) {
        let carried = methods
            .iter()
            .any(|method| method.rules.iter().any(|row| std::ptr::eq(*row, *rule)));
        assert_eq!(
            carried,
            !UNCARRIED_ROWS.contains(&rule.case),
            "row {} is carried by a method iff it is not an unallocated row",
            rule.case
        );
    }
    for kind in &kinds::KINDS {
        assert_eq!(
            kinds::kind_spec(kind.kind).map(|spec| spec.name),
            Some(kind.name)
        );
    }
}
