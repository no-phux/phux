//! Lowering agent badges and runtime events into [`WireEvent`].

#![allow(
    clippy::wildcard_imports,
    reason = "wire records intentionally share the parent UniFFI enum namespace"
)]

use super::*;

impl From<agent::AgentBadge> for WireEvent {
    fn from(value: agent::AgentBadge) -> Self {
        Self::AgentStateChanged {
            terminal_id: id::encode(&value.terminal_id),
            name: value.name,
            kind: value.kind,
            session: value.session,
            state: value.state.into(),
            attention: value.attention.into(),
            state_reading: value.state_reading.into(),
            attention_reading: value.attention_reading.into(),
        }
    }
}

/// Lower a runtime event that names a terminal signal or lifecycle fact, or
/// hand it back untouched.
///
/// Replica tombstones, progressive history and `Exited` lower to nothing:
/// a phone re-reads the pane, and the runtime follows `Exited` with a
/// `TerminalClosed` for a pane that is really gone, so a retained pane
/// (ADR-0124) must not disappear from the phone's list.
#[allow(
    clippy::result_large_err,
    reason = "the Err payload is the caller's own event handed back, not an error; boxing it would allocate once per drained event"
)]
pub(in crate::uniffi::wire) fn lower_event(event: Event) -> Result<Option<WireEvent>, Event> {
    let encode = |terminal_id: ResourceId| id::encode(&terminal_id);
    Ok(Some(match event {
        Event::Bell { terminal_id } => WireEvent::Bell {
            terminal_id: encode(terminal_id),
        },
        Event::ClipboardWrite { terminal_id, text } => WireEvent::ClipboardWrite {
            terminal_id: encode(terminal_id),
            text: text.0,
        },
        Event::TitleChanged { terminal_id, title } => WireEvent::TitleChanged {
            terminal_id: encode(terminal_id),
            title,
        },
        Event::CwdChanged { terminal_id, cwd } => WireEvent::CwdChanged {
            terminal_id: encode(terminal_id),
            cwd,
        },
        Event::OutputStarted { terminal_id } => WireEvent::OutputStarted {
            terminal_id: encode(terminal_id),
        },
        Event::OutputSettled { terminal_id } => WireEvent::OutputSettled {
            terminal_id: encode(terminal_id),
        },
        Event::CommandStarted { terminal_id } => WireEvent::CommandStarted {
            terminal_id: encode(terminal_id),
        },
        Event::CommandFinished {
            terminal_id,
            exit_code,
        } => WireEvent::CommandFinished {
            terminal_id: encode(terminal_id),
            exit_code,
        },
        Event::InputHolderChanged {
            terminal_id,
            holder,
            mine,
            ..
        } => WireEvent::InputHolderChanged {
            terminal_id: encode(terminal_id),
            holder: holder.map(phux_protocol::ClientId::get),
            mine,
        },
        Event::PaneSpawned { terminal_id } => WireEvent::PaneSpawned {
            terminal_id: encode(terminal_id),
        },
        Event::TerminalSpawned {
            request_id,
            terminal_id,
            error,
        } => WireEvent::TerminalSpawned {
            request_id,
            terminal_id: terminal_id.map(encode),
            error,
        },
        Event::TerminalAttached {
            request_id,
            terminal_id,
            error,
        } => WireEvent::TerminalAttached {
            request_id,
            terminal_id: encode(terminal_id),
            error,
        },
        Event::TerminalDetached {
            request_id,
            terminal_id,
            error,
        } => WireEvent::TerminalDetached {
            request_id,
            terminal_id: encode(terminal_id),
            error,
        },
        Event::TerminalClosed {
            terminal_id,
            exit_status,
            ..
        } => WireEvent::PaneClosed {
            terminal_id: encode(terminal_id),
            exit_status,
        },
        Event::ServerError { message, .. } => WireEvent::ServerError { message },
        Event::ResyncRequired { .. }
        | Event::History { .. }
        | Event::HistoryUnavailable { .. }
        | Event::Exited { .. }
        | Event::Detached { .. } => return Ok(None),
        other => return Err(other),
    }))
}
