//! Reliable, coalesced OSC 7501 publication through existing L3 metadata.

use std::collections::{HashMap, HashSet};

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    RESOURCE_AGENT_KEY, RESOURCE_PROGRAM_STATUS_KEY, RESOURCE_PROGRAM_STATUS_RECORD_PREFIX, Scope,
};
use tokio::sync::watch;

use crate::hooks::HookEvent;
use crate::state::{ServerState, SharedState};
use phux_agent_session::agent_record::AgentRecordJson;
use phux_terminal::actor::program_status::Record;

pub(in crate::runtime) fn spawn_drain(
    state: SharedState,
    terminal: ResourceId,
    mut rx: watch::Receiver<Vec<Record>>,
) {
    tokio::task::spawn_local(async move {
        let mut published = HashMap::new();
        let mut projection = None;
        loop {
            let hook = {
                let records = rx.borrow_and_update();
                state.with_mut(|s| {
                    s.resource_from_wire(&terminal)?;
                    publish(s, &terminal, &records, &mut published, &mut projection)
                })
            };
            // The hook re-takes the state lock, so it fires after release.
            if let Some(event) = hook {
                crate::hooks::fire_hook(&state, event);
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
    });
}

/// Records are written before the index, all under the same state lock.
/// Returns the `agent-state-changed` hook the projection owes, if any.
fn publish(
    s: &mut ServerState,
    terminal: &ResourceId,
    records: &[Record],
    published: &mut HashMap<String, Record>,
    projection: &mut Option<Projection>,
) -> Option<HookEvent> {
    let scope = Scope::Resource(terminal.clone());
    let next_ids: HashSet<&str> = records.iter().map(|record| record.id.as_str()).collect();
    published.retain(|id, _| {
        if next_ids.contains(id.as_str()) {
            true
        } else {
            s.metadata_delete(
                &scope,
                &format!("{RESOURCE_PROGRAM_STATUS_RECORD_PREFIX}{id}"),
            );
            false
        }
    });
    for record in records {
        if published.get(&record.id) == Some(record) {
            continue;
        }
        if let Ok(bytes) = serde_json::to_vec(record) {
            s.metadata_set(
                &scope,
                &format!("{RESOURCE_PROGRAM_STATUS_RECORD_PREFIX}{}", record.id),
                bytes,
            );
            published.insert(record.id.clone(), record.clone());
        }
    }
    let before = s
        .hook_dispatcher()
        .is_some()
        .then(|| s.metadata().get(&scope, RESOURCE_AGENT_KEY));
    if let Some(active) = records.first() {
        let index = Summary {
            record_ids: records.iter().map(|record| record.id.as_str()).collect(),
            active,
        };
        if let Ok(bytes) = serde_json::to_vec(&index) {
            s.metadata_set(&scope, RESOURCE_PROGRAM_STATUS_KEY, bytes);
        }
        project(s, terminal, &scope, active, projection);
    } else {
        s.metadata_delete(&scope, RESOURCE_PROGRAM_STATUS_KEY);
        clear_projection(s, terminal, &scope, projection);
    }
    // Hooks off: `before` was never read and nothing is owed.
    let after = s.metadata().get(&scope, RESOURCE_AGENT_KEY);
    HookEvent::agent_record_transition(terminal, before?.as_deref(), after.as_deref())
}

#[derive(serde::Serialize)]
struct Summary<'a> {
    record_ids: Vec<&'a str>,
    active: &'a Record,
}

struct Projection {
    bytes: Vec<u8>,
    /// Preserve an explicit identity's attention, not our previous error badge.
    attention: Option<String>,
}

fn project(
    s: &mut ServerState,
    terminal: &ResourceId,
    scope: &Scope,
    active: &Record,
    projection: &mut Option<Projection>,
) {
    if s.agent_records().is_declared(terminal) || s.resource_has_live_session(terminal) {
        return;
    }
    let existing = s.metadata().get(scope, RESOURCE_AGENT_KEY);
    let owned = s.agent_records().identity_ownership(terminal);
    let previous = existing.as_deref().and_then(AgentRecordJson::decode);
    let attention = if projection
        .as_ref()
        .is_some_and(|p| Some(p.bytes.as_slice()) == existing.as_deref())
    {
        projection.as_ref().and_then(|p| p.attention.clone())
    } else if owned.name {
        previous
            .as_ref()
            .and_then(|record| record.attention.clone())
    } else {
        None
    };
    let title = active.title.as_deref().map(display_text);
    let name = title
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .or(active.app.as_deref())
        .unwrap_or("Program");
    let state = if active.state == "error" {
        "done"
    } else {
        active.state
    };
    let kind = active.app.as_deref().unwrap_or("program");
    let mut record = previous.unwrap_or_default();
    let identity_only = record.state.is_empty();
    if record.name.is_empty() || !(owned.name || identity_only) {
        name.clone_into(&mut record.name);
    }
    if record.kind.as_ref().is_none_or(String::is_empty) || !(owned.kind || identity_only) {
        record.kind = Some(kind.to_owned());
    }
    state.clone_into(&mut record.state);
    record.attention = attention
        .clone()
        .or_else(|| (active.state == "error").then(|| "high".to_owned()));
    let bytes = record.encode();
    s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes.clone());
    *projection = Some(Projection { bytes, attention });
}

fn clear_projection(
    s: &mut ServerState,
    terminal: &ResourceId,
    scope: &Scope,
    projection: &mut Option<Projection>,
) {
    let Some(projection) = projection.take() else {
        return;
    };
    let existing = s.metadata().get(scope, RESOURCE_AGENT_KEY);
    if existing.as_deref() != Some(projection.bytes.as_slice())
        || s.agent_records().is_declared(terminal)
    {
        return;
    }
    if s.agent_records().has_explicit_identity(terminal)
        || s.agent_records().has_explicit_kind(terminal)
    {
        if let Some(mut record) = existing.as_deref().and_then(AgentRecordJson::decode) {
            "unknown".clone_into(&mut record.state);
            record.attention = projection.attention;
            s.metadata_set(scope, RESOURCE_AGENT_KEY, record.encode());
        }
    } else {
        s.metadata_delete(scope, RESOURCE_AGENT_KEY);
    }
    if let Some(pane) = s.resource_from_wire(terminal)
        && let Some(handle) = s.resource_handle(pane)
        && let Some(agent_parent) = handle.companion::<phux_agent_session::parent::AgentParent>()
    {
        let _ = agent_parent
            .mailbox
            .try_send(phux_agent_session::parent::AgentParentRequest::RecordInvalidated);
    }
}

/// Status labels outside the grid are plain text, without invisible formatting.
fn display_text(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !matches!(ch,
                '\u{00ad}' | '\u{034f}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' |
                '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' |
                '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e007f}'
            )
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::collections::HashMap;

    use super::{Record, publish};
    use crate::hooks::{AGENT_STATE_CHANGED, HookDispatcher};
    use crate::state::SharedState;

    fn record(state: &'static str) -> Record {
        Record {
            id: "build".to_owned(),
            state,
            kind: None,
            progress: None,
            app: Some("mytool".to_owned()),
            title: None,
            msg: None,
        }
    }

    /// phux-jqlh0.9: an OSC 7501 projection onto `phux.agent/v1` owes the
    /// `agent-state-changed` hook on each effective edge, and only then.
    #[test]
    fn program_status_projection_fires_agent_state_changed_on_real_edges() {
        let state = SharedState::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        state.with_mut(|s| s.set_hook_dispatcher(HookDispatcher::from_sender(tx)));
        let (_session, pane) = state.with_mut(|s| s.seed_session("osc-hook"));
        let terminal = state.with_mut(|s| s.intern_resource_wire(pane));
        let mut published = HashMap::new();
        let mut projection = None;
        let mut step = |records: &[Record]| {
            state.with_mut(|s| publish(s, &terminal, records, &mut published, &mut projection))
        };

        let first = step(&[record("working")]).expect("first sighting is an edge");
        assert_eq!(first.name, AGENT_STATE_CHANGED);
        assert_eq!(first.context.get("from"), None);
        assert_eq!(first.context.get("to").map(String::as_str), Some("working"));
        assert_eq!(
            first.context.get("agent-kind").map(String::as_str),
            Some("mytool")
        );
        assert!(step(&[record("working")]).is_none(), "no edge, no hook");
        let blocked = step(&[record("blocked")]).expect("working -> blocked");
        assert_eq!(
            blocked.context.get("from").map(String::as_str),
            Some("working")
        );
        assert_eq!(
            blocked.context.get("to").map(String::as_str),
            Some("blocked")
        );
        let cleared = step(&[]).expect("clearing the status withdraws the record");
        assert_eq!(
            cleared.context.get("from").map(String::as_str),
            Some("blocked")
        );
        assert_eq!(
            cleared.context.get("to").map(String::as_str),
            Some("unknown")
        );
        assert!(step(&[]).is_none(), "nothing left to withdraw");
        // `publish` only computes the hook; the drain fires it after release.
        assert!(rx.try_recv().is_err());
    }
}
