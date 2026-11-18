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
