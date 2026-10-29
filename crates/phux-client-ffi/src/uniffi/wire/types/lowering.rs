//! Lowering projected badges, signals, and lifecycle changes into [`WireEvent`].

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

impl From<event::TerminalSignal> for Option<WireEvent> {
    /// The Swift surface has no vocabulary for replica tombstones or
    /// progressive history, so those signals project to nothing. They are
    /// the runtime's business and the C lane's; a phone re-reads the pane.
    fn from(value: event::TerminalSignal) -> Self {
        Some(match value {
            event::TerminalSignal::Bell { terminal_id } => WireEvent::Bell {
                terminal_id: id::encode(&terminal_id),
            },
            event::TerminalSignal::ClipboardWrite { terminal_id, text } => {
                WireEvent::ClipboardWrite {
                    terminal_id: id::encode(&terminal_id),
                    text: text.0,
                }
            }
            event::TerminalSignal::TitleChanged { terminal_id, title } => WireEvent::TitleChanged {
                terminal_id: id::encode(&terminal_id),
                title,
            },
            event::TerminalSignal::CwdChanged { terminal_id, cwd } => WireEvent::CwdChanged {
                terminal_id: id::encode(&terminal_id),
                cwd,
            },
            event::TerminalSignal::OutputStarted { terminal_id } => WireEvent::OutputStarted {
                terminal_id: id::encode(&terminal_id),
            },
            event::TerminalSignal::OutputSettled { terminal_id } => WireEvent::OutputSettled {
                terminal_id: id::encode(&terminal_id),
            },
            event::TerminalSignal::CommandStarted { terminal_id } => WireEvent::CommandStarted {
                terminal_id: id::encode(&terminal_id),
            },
            event::TerminalSignal::CommandFinished {
                terminal_id,
                exit_code,
            } => WireEvent::CommandFinished {
                terminal_id: id::encode(&terminal_id),
                exit_code,
            },
            event::TerminalSignal::InputHolder {
                terminal_id,
                holder,
                mine,
                ..
            } => WireEvent::InputHolderChanged {
                terminal_id: id::encode(&terminal_id),
                holder: holder.map(phux_protocol::ClientId::get),
                mine,
            },
            event::TerminalSignal::Resync { .. }
            | event::TerminalSignal::History { .. }
            | event::TerminalSignal::HistoryUnavailable { .. } => return None,
        })
    }
}

impl From<event::Lifecycle> for Option<WireEvent> {
    /// `Exited` projects to nothing: the runtime follows it with a
    /// `Closed` for a pane that is really gone, and a retained pane
    /// (ADR-0124) must not disappear from the phone's list.
    fn from(value: event::Lifecycle) -> Self {
        Some(match value {
            event::Lifecycle::PaneSpawned { terminal_id } => WireEvent::PaneSpawned {
                terminal_id: id::encode(&terminal_id),
            },
            event::Lifecycle::SpawnAnswered {
                request_id,
                terminal_id,
                error,
            } => WireEvent::TerminalSpawned {
                request_id,
                terminal_id: terminal_id.as_ref().map(id::encode),
                error,
            },
            event::Lifecycle::AttachAnswered {
                request_id,
                terminal_id,
                error,
            } => WireEvent::TerminalAttached {
                request_id,
                terminal_id: id::encode(&terminal_id),
                error,
            },
            event::Lifecycle::DetachAnswered {
                request_id,
                terminal_id,
                error,
            } => WireEvent::TerminalDetached {
                request_id,
                terminal_id: id::encode(&terminal_id),
                error,
            },
            event::Lifecycle::Closed {
                terminal_id,
                exit_status,
                ..
            } => WireEvent::PaneClosed {
                terminal_id: id::encode(&terminal_id),
                exit_status,
            },
            event::Lifecycle::ServerError { message, .. } => WireEvent::ServerError { message },
            event::Lifecycle::Exited { .. } | event::Lifecycle::Detached { .. } => return None,
        })
    }
}
