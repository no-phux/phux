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
fn replica_and_history_signals_have_no_swift_vocabulary() {
    let signal = event::TerminalSignal::HistoryUnavailable {
        terminal_id: ResourceId::local(1),
        reason: phux_client_core::session::HistoryUnavailableReason::Pruned,
    };
    assert_eq!(Option::<WireEvent>::from(signal), None);
}

#[test]
fn a_burst_signal_lowers_to_its_event() {
    let signal = event::TerminalSignal::OutputStarted {
        terminal_id: ResourceId::local(1),
    };
    assert_eq!(
        Option::<WireEvent>::from(signal),
        Some(WireEvent::OutputStarted {
            terminal_id: "local:1".to_owned(),
        })
    );
}

#[test]
fn a_close_becomes_a_pane_closed_and_an_exit_does_not() {
    let closed = event::Lifecycle::Closed {
        terminal_id: ResourceId::local(4),
        exit_status: Some(1),
        signal: None,
        reason: CloseReason::Exited,
    };
    assert_eq!(
        Option::<WireEvent>::from(closed),
        Some(WireEvent::PaneClosed {
            terminal_id: "local:4".to_owned(),
            exit_status: Some(1),
        })
    );
    let exited = event::Lifecycle::Exited {
        terminal_id: ResourceId::local(4),
        exit_status: Some(1),
        signal: None,
        reason: CloseReason::Exited,
    };
    assert_eq!(Option::<WireEvent>::from(exited), None);
}

#[test]
fn a_spawn_answer_keeps_its_optional_terminal() {
    let answered = event::Lifecycle::SpawnAnswered {
        request_id: 3,
        terminal_id: None,
        error: Some("refused".to_owned()),
    };
    assert_eq!(
        Option::<WireEvent>::from(answered),
        Some(WireEvent::TerminalSpawned {
            request_id: 3,
            terminal_id: None,
            error: Some("refused".to_owned()),
        })
    );
}

#[test]
fn statuses_fold_the_pre_attach_states_together() {
    assert_eq!(
        WireStatus::from(status::Connection::Connecting),
        WireStatus::Connecting
    );
    assert_eq!(
        WireStatus::from(status::Connection::Failed),
        WireStatus::Failed
    );
}
