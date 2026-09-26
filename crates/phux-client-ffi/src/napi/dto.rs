//! Mechanical encoding of binding-neutral projection values. No grid DTO:
//! native painters acquire immutable frames from the shared Rust Client.

use napi_derive::napi;
use phux_client_runtime::control::Event;

use crate::projection::{event, id, outcome, status, topology};

#[napi(string_enum)]
#[derive(Debug)]
pub enum DesktopStatus {
    Connecting,
    Attached,
    Closed,
    Failed,
}

impl From<status::Connection> for DesktopStatus {
    fn from(value: status::Connection) -> Self {
        match value {
            status::Connection::Connecting => Self::Connecting,
            status::Connection::Attached => Self::Attached,
            status::Connection::Closed => Self::Closed,
            status::Connection::Failed => Self::Failed,
        }
    }
}

#[napi(string_enum)]
#[derive(Debug)]
pub enum DesktopDelivery {
    Delivered,
    Refused,
    Unknown,
}

impl From<outcome::Delivery> for DesktopDelivery {
    fn from(value: outcome::Delivery) -> Self {
        match value {
            outcome::Delivery::Delivered => Self::Delivered,
            outcome::Delivery::Refused => Self::Refused,
            outcome::Delivery::Unknown => Self::Unknown,
        }
    }
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopSession {
    pub id: u32,
    pub name: String,
    pub window_count: u16,
    pub attached_client_count: u16,
}

impl From<topology::Session> for DesktopSession {
    fn from(value: topology::Session) -> Self {
        Self {
            id: value.id,
            name: value.name,
            window_count: value.window_count,
            attached_client_count: value.attached_client_count,
        }
    }
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopPane {
    pub terminal_id: String,
    pub session_id: u32,
    pub session_name: String,
    pub window_id: u32,
    pub window_index: u16,
    pub window_name: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub is_focused: bool,
}

impl From<topology::Pane> for DesktopPane {
    fn from(value: topology::Pane) -> Self {
        Self {
            terminal_id: value.terminal_id,
            session_id: value.session_id,
            session_name: value.session_name,
            window_id: value.window_id,
            window_index: value.window_index,
            window_name: value.window_name,
            title: value.title,
            cwd: value.cwd,
            is_focused: value.is_focused,
        }
    }
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopTopology {
    pub sessions: Vec<DesktopSession>,
    pub panes: Vec<DesktopPane>,
    pub focused_pane: String,
}

impl From<topology::SessionGraph> for DesktopTopology {
    fn from(value: topology::SessionGraph) -> Self {
        Self {
            sessions: value.sessions.into_iter().map(Into::into).collect(),
            panes: value.panes.into_iter().map(Into::into).collect(),
            focused_pane: value.focused_pane,
        }
    }
}

/// A readiness snapshot, not permission to retry an unknown input. The
/// runtime checks the gate again atomically when applying acknowledged input.
#[napi(object)]
#[derive(Debug)]
pub struct DesktopInputReadiness {
    pub ready: bool,
    pub delivery_fenced: bool,
}

/// Control-plane events only. Strings carry every 64-bit correlation exactly.
#[napi(discriminant = "kind")]
#[derive(Debug)]
pub enum DesktopEvent {
    StatusChanged {
        status: DesktopStatus,
    },
    TopologyChanged {},
    TerminalChanged {
        terminal_id: String,
    },
    AgentBadge {
        terminal_id: String,
        name: String,
        agent_kind: Option<String>,
        state: String,
        attention: String,
    },
    PaneSpawned {
        terminal_id: String,
    },
    SpawnAnswered {
        request_id: u32,
        terminal_id: Option<String>,
        error: Option<String>,
    },
    AttachAnswered {
        request_id: u32,
        terminal_id: String,
        error: Option<String>,
    },
    DetachAnswered {
        request_id: u32,
        terminal_id: String,
        error: Option<String>,
    },
    TerminalKilled {
        request_id: u32,
        terminal_id: String,
        error: Option<String>,
    },
    Closed {
        terminal_id: String,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: u16,
    },
    Exited {
        terminal_id: String,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: u16,
    },
    Detached {
        reason: Option<u16>,
        message: String,
    },
    ServerError {
        code: u16,
        message: String,
        request_id: Option<u32>,
    },
    InputDelivery {
        delivery_id: String,
        outcome: DesktopDelivery,
        code: Option<u16>,
        message: String,
    },
}

impl From<event::Lifecycle> for DesktopEvent {
    fn from(value: event::Lifecycle) -> Self {
        use event::Lifecycle;
        match value {
            Lifecycle::PaneSpawned { terminal_id } => Self::PaneSpawned {
                terminal_id: id::encode(&terminal_id),
            },
            Lifecycle::SpawnAnswered {
                request_id,
                terminal_id,
                error,
            } => Self::SpawnAnswered {
                request_id,
                terminal_id: terminal_id.as_ref().map(id::encode),
                error,
            },
            Lifecycle::AttachAnswered {
                request_id,
                terminal_id,
                error,
            } => Self::AttachAnswered {
                request_id,
                terminal_id: id::encode(&terminal_id),
                error,
            },
            Lifecycle::DetachAnswered {
                request_id,
                terminal_id,
                error,
            } => Self::DetachAnswered {
                request_id,
                terminal_id: id::encode(&terminal_id),
                error,
            },
            Lifecycle::Closed {
                terminal_id,
                exit_status,
                signal,
                reason,
            } => Self::Closed {
                terminal_id: id::encode(&terminal_id),
                exit_status,
                signal,
                reason: reason as u16,
            },
            Lifecycle::Exited {
                terminal_id,
                exit_status,
                signal,
                reason,
            } => Self::Exited {
                terminal_id: id::encode(&terminal_id),
                exit_status,
                signal,
                reason: reason as u16,
            },
            Lifecycle::Detached { reason, message } => Self::Detached {
                reason: reason.map(|value| value as u16),
                message,
            },
            Lifecycle::ServerError {
                code,
                message,
                request_id,
            } => Self::ServerError {
                code: code as u16,
                message,
                request_id,
            },
        }
    }
}

pub(super) fn encode_event(value: Event) -> Option<DesktopEvent> {
    let value = match event::lifecycle(value) {
        Ok(lifecycle) => return Some(lifecycle.into()),
        Err(value) => value,
    };
    encode_activity(value)
}

fn encode_agent_frame(frame: phux_protocol::wire::frame::FrameKind) -> Option<DesktopEvent> {
    use phux_protocol::wire::frame::{FrameKind, RESOURCE_AGENT_KEY, Scope};
    match frame {
        FrameKind::MetadataChanged {
            scope: Scope::Resource(id),
            key,
            value,
            ..
        } if key == RESOURCE_AGENT_KEY => Some(agent_badge(&id, value.as_deref())),
        FrameKind::MetadataValue { request_id, value } => {
            super::take_agent_watch(request_id).map(|id| agent_badge(&id, value.as_deref()))
        }
        _ => None,
    }
}

fn agent_badge(id: &phux_protocol::ResourceId, bytes: Option<&[u8]>) -> DesktopEvent {
    let badge = crate::projection::agent::badge(id, bytes);
    DesktopEvent::AgentBadge {
        terminal_id: id::encode(&badge.terminal_id),
        name: badge.name,
        agent_kind: badge.kind,
        state: format!("{:?}", badge.state).to_lowercase(),
        attention: format!("{:?}", badge.attention).to_lowercase(),
    }
}

fn encode_activity(value: Event) -> Option<DesktopEvent> {
    match value {
        Event::TerminalKilled {
            request_id,
            terminal_id,
            error,
        } => Some(DesktopEvent::TerminalKilled {
            request_id,
            terminal_id: id::encode(&terminal_id),
            error,
        }),
        Event::StatusChanged(value) => Some(DesktopEvent::StatusChanged {
            status: status::connection(Some(value)).into(),
        }),
        Event::TopologyChanged => Some(DesktopEvent::TopologyChanged {}),
        Event::TerminalChanged { terminal_id } => Some(DesktopEvent::TerminalChanged {
            terminal_id: id::encode(&terminal_id),
        }),
        Event::Frame(frame) => encode_agent_frame(*frame),
        Event::InputDelivery {
            delivery_id,
            outcome: value,
            code,
            message,
        } => Some(DesktopEvent::InputDelivery {
            delivery_id: delivery_id.to_string(),
            outcome: outcome::delivery(value).into(),
            code,
            message,
        }),
        // No terminal output, grid cells, or unrelated wire frames cross JS.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopDelivery, DesktopEvent, encode_event};
    use phux_client_runtime::control::{DeliveryOutcome, Event};

    #[test]
    fn kill_success_and_refusal_keep_exact_resource_and_request() {
        for error in [None, Some("scope refused".to_owned())] {
            let encoded = encode_event(Event::TerminalKilled {
                request_id: u32::MAX,
                terminal_id: phux_protocol::ResourceId::local(42),
                error: error.clone(),
            });
            assert!(matches!(encoded, Some(DesktopEvent::TerminalKilled {
                request_id: u32::MAX, terminal_id, error: actual,
            }) if terminal_id == "local:42" && actual == error));
        }
    }

    #[test]
    fn unknown_delivery_keeps_its_uncertainty_and_full_width_correlation() {
        let encoded = encode_event(Event::InputDelivery {
            delivery_id: u64::MAX,
            outcome: DeliveryOutcome::Unknown,
            code: None,
            message: "connection lost before receipt".to_owned(),
        });
        assert!(matches!(encoded, Some(DesktopEvent::InputDelivery {
            delivery_id,
            outcome: DesktopDelivery::Unknown,
            code: None,
            ..
        }) if delivery_id == "18446744073709551615"));
    }
}
