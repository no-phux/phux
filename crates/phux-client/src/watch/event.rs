//! A streamed event and its stable JSON representation.

use phux_protocol::ids::{ClientId, ResourceId};
use phux_protocol::wire::frame::{AgentEvent, EventStamp};
use serde_json::{Map, Value, json};

use crate::selector::format_terminal_id;

/// One streamed agent event plus the Terminal it concerns (`None` for a
/// server-scoped event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    /// The Terminal the event concerns, or `None` if server-scoped.
    pub terminal: Option<ResourceId>,
    /// The event payload.
    pub event: AgentEvent,
    /// The journal stamp (ADR-0123); `None` from an unstamping peer and on a
    /// `journal_gap` notice.
    pub stamp: Option<EventStamp>,
}

impl WatchEvent {
    /// Unwrap one `EVENT` frame's fields.
    pub(super) fn from_frame(
        terminal: Option<ResourceId>,
        event: AgentEvent,
        stamp: Option<Box<EventStamp>>,
    ) -> Self {
        Self {
            terminal,
            event,
            stamp: stamp.map(|stamp| *stamp),
        }
    }
}

/// The stable `event` name `phux watch --json`, `--until`, and MCP use; a
/// tag this build predates is `unknown`.
#[must_use]
pub const fn event_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::CommandStarted => "command_started",
        AgentEvent::CommandFinished { .. } => "command_finished",
        AgentEvent::TitleChanged { .. } => "title_changed",
        AgentEvent::Bell => "bell",
        AgentEvent::ResourceSpawned { .. } => "pane_spawned",
        AgentEvent::ResourceClosed { .. } => "pane_closed",
        AgentEvent::Dirty => "dirty",
        AgentEvent::Idle => "idle",
        AgentEvent::Asked { .. } => "asked",
        AgentEvent::TerminalControl { .. } => "terminal_control",
        AgentEvent::CwdChanged { .. } => "cwd_changed",
        AgentEvent::JournalGap { .. } => "journal_gap",
        AgentEvent::SourceGap { .. } => "source_gap",
        AgentEvent::ApprovalRequested { .. } => "approval_requested",
        AgentEvent::ApprovalDecided { .. } => "approval_decided",
        _ => "unknown",
    }
}

/// One watch event as the JSON object `phux watch --json` and MCP print:
/// `event`, `terminal`, the payload fields, then the journal stamp.
#[must_use]
pub fn event_json(ev: &WatchEvent) -> Value {
    let mut obj = Map::new();
    obj.insert("event".to_owned(), Value::from(event_name(&ev.event)));
    if let Some(terminal) = &ev.terminal {
        obj.insert(
            "terminal".to_owned(),
            Value::from(format_terminal_id(terminal)),
        );
    }
    if let Value::Object(payload) = payload_json(&ev.event) {
        obj.extend(payload);
    }
    if let Some(stamp) = &ev.stamp {
        insert_stamp(&mut obj, stamp);
    }
    Value::Object(obj)
}

/// The payload fields of `event`, as a JSON object.
fn payload_json(event: &AgentEvent) -> Value {
    match event {
        AgentEvent::TitleChanged { title } => json!({ "title": title }),
        AgentEvent::CommandFinished { exit_code } => json!({ "exit_code": exit_code }),
        AgentEvent::ResourceClosed { exit_status } => json!({ "exit_status": exit_status }),
        // Additive (ADR-0102): the spawned resource's kind and parent, so a
        // consumer can tell a new pane from a new agent session bound to one.
        AgentEvent::ResourceSpawned { kind, parent } => json!({
            "kind": kind.as_str(),
            "parent": parent.as_ref().map(format_terminal_id),
        }),
        AgentEvent::Asked {
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => json!({
            "id": id,
            "question": question,
            "suggestions": suggestions,
            "elapsed_seconds": elapsed_seconds,
        }),
        AgentEvent::TerminalControl { .. } => terminal_control_json(event),
        AgentEvent::CwdChanged { cwd } => json!({ "cwd": cwd }),
        AgentEvent::JournalGap {
            first_missing,
            last_missing,
        } => json!({ "first_missing": first_missing, "last_missing": last_missing }),
        AgentEvent::SourceGap { dropped } => json!({ "dropped": dropped }),
        // ADR-0128: the id names the `phux.approval/v1/<id>` record, which
        // holds the details while the action is pending.
        AgentEvent::ApprovalRequested { id } => json!({ "id": id.to_string() }),
        AgentEvent::ApprovalDecided { id, outcome } => {
            json!({ "id": id.to_string(), "outcome": outcome.as_str() })
        }
        AgentEvent::Unknown { tag, .. } => json!({ "tag": tag }),
        _ => json!({}),
    }
}

/// `terminal_control`'s payload. Its own connection id rides as
/// `actor_client`, so it cannot collide with the journal stamp's `actor`.
fn terminal_control_json(event: &AgentEvent) -> Value {
    let AgentEvent::TerminalControl {
        lifecycle,
        exit_status,
        input_holder,
        action,
        actor,
    } = event
    else {
        return json!({});
    };
    json!({
        "lifecycle": crate::resource::lifecycle_name(*lifecycle),
        "action": crate::resource::control_action_name(*action),
        "exit_status": exit_status,
        "input_holder": input_holder.map(ClientId::get),
        "actor_client": actor.map(ClientId::get),
    })
}

fn insert_stamp(obj: &mut Map<String, Value>, stamp: &EventStamp) {
    obj.insert("seq".to_owned(), Value::from(stamp.seq));
    obj.insert("ts_ms".to_owned(), Value::from(stamp.ts_ms));
    if let Some(actor) = &stamp.actor {
        obj.insert(
            "actor".to_owned(),
            json!({
                "client": actor.client.get(),
                "credential_id": actor.credential_id,
                "client_name": actor.client_name,
            }),
        );
    }
}
