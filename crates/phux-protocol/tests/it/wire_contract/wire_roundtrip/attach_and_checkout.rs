//! Attach golden bytes, agent cursors, and checkout payloads.

use super::{
    BytesMut, Command, CommandResult, CommandValue, DecodeError, FrameKind, ResourceId,
    ViewportInfo, framed_tlv, tlv_field,
};

/// ADR-0127: a session `ATTACH` with no role is byte-for-byte the frame
/// from before roles; field 6 is written only for `Some`.
#[test]
fn session_attach_without_a_role_matches_its_golden_bytes() {
    use std::fmt::Write as _;
    const GOLDEN: &str =
        "00000028020104090100000004776f726b020406005000180000030401010404040000271005040400000007";
    let frame = FrameKind::Attach {
        attach_id: 7,
        target: phux_protocol::wire::frame::AttachTarget::ByName("work".to_owned()),
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: true,
        scrollback_limit_lines: 10_000,
        role_policy: None,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let mut hex = String::new();
    for byte in &buf {
        let _ = write!(hex, "{byte:02x}");
    }
    assert_eq!(hex, GOLDEN);
}

#[test]
fn agent_attach_role_and_cursor_each_sit_behind_a_presence_byte() {
    use phux_protocol::wire::frame::RolePolicy;
    let frame = |role_policy, agent_after_seq| FrameKind::Command {
        request_id: 9,
        command: Command::AttachResource {
            terminal_id: ResourceId::local(7),
            role_policy,
            agent_after_seq,
        },
    };
    let golden = |body: &[u8]| {
        let mut fields = Vec::new();
        tlv_field(&mut fields, 1, &9u32.to_be_bytes());
        tlv_field(&mut fields, 2, body);
        framed_tlv(0x31, &fields)
    };
    let cursor = 0x0102_0304_0506_0708u64;
    for (role, role_bytes) in [
        (None, vec![0]),
        (Some(RolePolicy::PRIMARY), vec![1, 0]),
        (Some(RolePolicy::VIEWER), vec![1, 1]),
        (Some(RolePolicy::TAKEOVER), vec![1, 2]),
    ] {
        for after_seq in [None, Some(cursor)] {
            let mut body = vec![1, 0, 0, 0, 0, 7];
            body.extend_from_slice(&role_bytes);
            match after_seq {
                None => body.push(0),
                Some(seq) => {
                    body.push(1);
                    body.extend_from_slice(&seq.to_be_bytes());
                }
            }
            let mut encoded = BytesMut::new();
            frame(role, after_seq).encode(&mut encoded);
            assert_eq!(encoded.as_ref(), golden(&body));
            assert_eq!(
                FrameKind::decode(&encoded).unwrap().0,
                frame(role, after_seq)
            );
            // Bytes a newer build appends are skipped, not misread.
            body.extend_from_slice(&[0xEE; 9]);
            assert_eq!(
                FrameKind::decode(&golden(&body)).unwrap().0,
                frame(role, after_seq)
            );
        }
    }
    for length in 0..8 {
        let mut partial = vec![1, 0, 0, 0, 0, 7, 0, 1];
        partial.extend(std::iter::repeat_n(0, length));
        assert!(
            FrameKind::decode(&golden(&partial)).is_err(),
            "partial cursor: {length}"
        );
    }
}

#[test]
fn checkout_requests_and_answers_round_trip_and_are_bounded() {
    use phux_protocol::checkout::{
        CheckoutAnswer, CheckoutArea, CheckoutEntry, CheckoutRefusal, CheckoutRefusalCode,
        CheckoutRequest, CheckoutStatus, MAX_CHECKOUT_REQUEST_BYTES,
    };
    let terminal = phux_protocol::ids::ResourceId::satellite("mini", 7);
    for request in [
        CheckoutRequest::Status,
        CheckoutRequest::Diff {
            path: "src/main.rs".to_owned(),
            area: CheckoutArea::Untracked,
        },
        CheckoutRequest::Log {
            skip: 100,
            limit: 50,
            path: Some("README.md".to_owned()),
        },
        CheckoutRequest::Tree { dir: String::new() },
        CheckoutRequest::PullRequest,
        CheckoutRequest::Unstage {
            paths: vec!["a b.txt".to_owned(), "ü.rs".to_owned()],
        },
    ] {
        let frame = FrameKind::Command {
            request_id: 9,
            command: Command::Checkout {
                terminal_id: terminal.clone(),
                request,
            },
        };
        let mut encoded = BytesMut::new();
        frame.encode(&mut encoded);
        let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
        assert_eq!(decoded, frame);
        assert!(tail.is_empty());
    }

    for answer in [
        CheckoutAnswer::Status(CheckoutStatus {
            root: "/work/phux".to_owned(),
            branch: Some("main".to_owned()),
            ahead: 1,
            entries: vec![CheckoutEntry {
                path: "new.rs".to_owned(),
                from: Some("old.rs".to_owned()),
                area: CheckoutArea::Staged,
                code: "R".to_owned(),
                added: Some(2),
                deleted: None,
                binary: false,
            }],
            ..CheckoutStatus::default()
        }),
        CheckoutAnswer::Refused(CheckoutRefusal {
            code: CheckoutRefusalCode::NotARepository,
            message: "not a checkout".to_owned(),
        }),
    ] {
        let reply = FrameKind::CommandResult {
            request_id: 9,
            result: CommandResult::OkWith(CommandValue::Checkout(Box::new(answer))),
        };
        let mut encoded = BytesMut::new();
        reply.encode(&mut encoded);
        let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
        assert_eq!(decoded, reply);
        assert!(tail.is_empty());
    }

    let oversized = FrameKind::Command {
        request_id: 10,
        command: Command::Checkout {
            terminal_id: terminal,
            request: CheckoutRequest::Stage {
                paths: vec!["x".repeat(MAX_CHECKOUT_REQUEST_BYTES)],
            },
        },
    };
    let mut bytes = BytesMut::new();
    oversized.encode(&mut bytes);
    assert_eq!(
        FrameKind::decode(&bytes).unwrap_err(),
        DecodeError::CheckoutLimitExceeded
    );
}
