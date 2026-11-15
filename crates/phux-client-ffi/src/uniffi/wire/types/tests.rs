//! Wire record lowering tests.

use super::WireTerminalTheme;
use crate::uniffi::engine::Color;

#[test]
fn a_wire_theme_needs_exactly_sixteen_ansi_entries() {
    let grey = |v| Color { r: v, g: v, b: v };
    let theme = |n: u8| WireTerminalTheme {
        ansi16: (0..n).map(grey).collect(),
        foreground: grey(250),
        background: grey(10),
        cursor: grey(200),
    };
    let ok = phux_client_runtime::engine::TerminalTheme::try_from(theme(16)).unwrap();
    assert_eq!(ok.ansi16[15], [15; 3]);
    assert_eq!(ok.background, [10; 3]);
    for n in [0, 15, 17] {
        assert!(phux_client_runtime::engine::TerminalTheme::try_from(theme(n)).is_err());
    }
}

use super::*;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::CloseReason;

#[test]
fn a_badge_lowers_with_its_string_identity() {
    let badge = agent::badge(
        &ResourceId::local(9),
        Some(br#"{"name":"Claude","kind":"claude","state":"newer"}"#),
    );
    assert_eq!(
        WireEvent::from(badge),
        WireEvent::AgentStateChanged {
            terminal_id: "local:9".to_owned(),
            name: "Claude".to_owned(),
            kind: Some("claude".to_owned()),
            session: None,
            state: AgentState::Unknown,
            attention: AgentAttention::Low,
            state_reading: AgentStateReading::Unsupported {
                raw: "newer".to_owned()
            },
            attention_reading: AgentAttentionReading::Derived,
        }
    );
}

#[test]
fn readings_lower_one_for_one() {
    for (record, state, attention) in [
        (
            r#"{"name":"a","state":"unknown","attention":"high"}"#,
            AgentStateReading::Indeterminate,
            AgentAttentionReading::Declared,
        ),
        (
            r#"{"name":"a","attention":"urgent"}"#,
            AgentStateReading::Undeclared,
            AgentAttentionReading::Unsupported {
                raw: "urgent".to_owned(),
            },
        ),
        (
            r#"{"name":"a","state":"working"}"#,
            AgentStateReading::Recognized,
            AgentAttentionReading::Derived,
        ),
    ] {
        let WireEvent::AgentStateChanged {
            state_reading,
            attention_reading,
            ..
        } = WireEvent::from(agent::badge(&ResourceId::local(1), Some(record.as_bytes())))
        else {
            panic!("a badge lowers to AgentStateChanged");
        };
        assert_eq!(state_reading, state, "{record}");
        assert_eq!(attention_reading, attention, "{record}");
    }
}

#[test]
fn a_malformed_record_lowers_to_an_empty_badge() {
    let badge = agent::badge(&ResourceId::local(2), Some(b"not-json"));
    assert_eq!(
        WireEvent::from(badge),
        WireEvent::AgentStateChanged {
            terminal_id: "local:2".to_owned(),
            name: String::new(),
            kind: None,
            session: None,
            state: AgentState::Unknown,
            attention: AgentAttention::Low,
            state_reading: AgentStateReading::NoRecord,
            attention_reading: AgentAttentionReading::Derived,
        }
    );
}

#[test]
fn replica_history_exit_and_detach_events_have_no_swift_vocabulary() {
    let local = ResourceId::local(1);
    for event in [
        Event::HistoryUnavailable {
            terminal_id: local.clone(),
            reason: phux_client_core::session::HistoryUnavailableReason::Pruned,
        },
        Event::Exited {
            terminal_id: local,
            exit_status: Some(1),
            signal: None,
            reason: CloseReason::Exited,
        },
    ] {
        assert_eq!(lower_event(event).ok(), Some(None));
    }
}

#[test]
fn signals_and_lifecycle_events_lower_with_string_identities() {
    let cases = [
        (
            Event::OutputStarted {
                terminal_id: ResourceId::local(1),
            },
            WireEvent::OutputStarted {
                terminal_id: "local:1".to_owned(),
            },
        ),
        (
            Event::TitleChanged {
                terminal_id: ResourceId::satellite("box", 2),
                title: "vim".to_owned(),
            },
            WireEvent::TitleChanged {
                terminal_id: "satellite:box:2".to_owned(),
                title: "vim".to_owned(),
            },
        ),
        (
            Event::TerminalClosed {
                terminal_id: ResourceId::local(4),
                exit_status: Some(1),
                signal: None,
                reason: CloseReason::Exited,
            },
            WireEvent::PaneClosed {
                terminal_id: "local:4".to_owned(),
                exit_status: Some(1),
            },
        ),
        (
            Event::TerminalSpawned {
                request_id: 3,
                terminal_id: None,
                error: Some("refused".to_owned()),
            },
            WireEvent::TerminalSpawned {
                request_id: 3,
                terminal_id: None,
                error: Some("refused".to_owned()),
            },
        ),
    ];
    for (event, lowered) in cases {
        assert_eq!(lower_event(event).ok(), Some(Some(lowered)));
    }
}

#[test]
fn unrelated_events_are_handed_back() {
    let event = lower_event(Event::TerminalChanged {
        terminal_id: ResourceId::local(5),
    })
    .expect_err("not lowered here");
    assert!(matches!(event, Event::TerminalChanged { .. }));
}

#[test]
fn statuses_fold_the_pre_attach_states_together() {
    use phux_client_runtime::control::Status;
    for status in [
        None,
        Some(Status::Idle),
        Some(Status::Connecting),
        Some(Status::Negotiated),
    ] {
        assert_eq!(WireStatus::from(status), WireStatus::Connecting);
    }
    assert_eq!(
        WireStatus::from(Some(Status::Attached)),
        WireStatus::Attached
    );
    assert_eq!(WireStatus::from(Some(Status::Closed)), WireStatus::Closed);
    assert_eq!(WireStatus::from(Some(Status::Failed)), WireStatus::Failed);
}

#[test]
fn path_answers_keep_kind_status_and_cancellation() {
    use phux_client_runtime::control::{
        PathAnswer, PathFailure, PathMatch, PathMatchKind, PathSearchStatus,
    };
    let answer = WirePathAnswer::from(PathAnswer {
        request_id: 17,
        root: "/root".into(),
        parent: Some("/".into()),
        rows: vec![PathMatch {
            path: "/root/a b".into(),
            kind: PathMatchKind::Symlink,
        }],
        status: Some(PathSearchStatus::Truncated),
        failure: None,
        message: String::new(),
    });
    assert_eq!(answer.rows[0].kind, WirePathKind::Symlink);
    assert_eq!(answer.status, Some(WirePathStatus::Truncated));
    assert_eq!(answer.error, None);
    let cancelled = WirePathAnswer::from(PathAnswer {
        request_id: 18,
        root: "/root".into(),
        parent: None,
        rows: Vec::new(),
        status: None,
        failure: Some(PathFailure::Unanswered),
        message: "disconnected".into(),
    });
    assert_eq!(cancelled.error, Some(WirePathError::Unanswered));
}

#[test]
fn a_listing_carries_its_entries_truncation_and_failure() {
    use phux_client_runtime::control::{DirectoryChild, DirectoryFailure, DirectoryListing};
    let listing = WireDirectoryListing::from(DirectoryListing {
        request_id: 3,
        path: "/home".to_owned(),
        parent: Some("/".to_owned()),
        entries: vec![DirectoryChild {
            name: "work".to_owned(),
            is_symlink: true,
        }],
        truncated: true,
        error: Some(DirectoryFailure::Unanswered),
        message: String::new(),
    });
    assert!(listing.truncated);
    assert_eq!(listing.entries[0].name, "work");
    assert!(listing.entries[0].is_symlink);
    assert_eq!(listing.error, Some(WireDirectoryErrorCode::Unanswered));
}
