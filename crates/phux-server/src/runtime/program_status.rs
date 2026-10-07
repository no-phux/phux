//! Reliable, coalesced OSC 7501 publication through existing L3 metadata.

use std::collections::{HashMap, HashSet};

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    RESOURCE_AGENT_KEY, RESOURCE_PROGRAM_STATUS_KEY, RESOURCE_PROGRAM_STATUS_RECORD_PREFIX, Scope,
};
use tokio::sync::watch;

use crate::agent_detect::record::AgentRecordJson;
use crate::resource::terminal::program_status::Record;
use crate::state::{ServerState, SharedState};

pub(super) fn spawn_drain(
    state: SharedState,
    terminal: ResourceId,
    mut rx: watch::Receiver<Vec<Record>>,
) {
    tokio::task::spawn_local(async move {
        let mut published = HashMap::new();
        let mut projection = None;
        loop {
            {
                let records = rx.borrow_and_update();
                state.with_mut(|s| {
                    if s.terminal_from_wire(&terminal).is_some() {
                        publish(s, &terminal, &records, &mut published, &mut projection);
                    }
                });
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
    });
}

/// Records are written before the index, all under the same state lock.
fn publish(
    s: &mut ServerState,
    terminal: &ResourceId,
    records: &[Record],
    published: &mut HashMap<String, Record>,
    projection: &mut Option<Projection>,
) {
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
    if s.agent_records().is_declared(terminal)
        || crate::agent_detect::live_session::server_has_live_session(s, terminal)
    {
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
    if let Some(pane) = s.terminal_from_wire(terminal)
        && let Some(handle) = s.resource_handle(pane)
    {
        let _ = handle
            .control
            .try_send(crate::resource::ControlRequest::AgentRecordInvalidated);
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
