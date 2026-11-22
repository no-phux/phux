//! The per-Terminal metadata a watch follows: the `phux.agent/v1` record and
//! the `phux.controller/v1` record (`docs/building.md#who-is-typing`).

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, RESOURCE_CONTROLLER_KEY, Scope};
use serde_json::{Value, json};

use super::AgentStateUpdate;
use crate::agent_meta::{AgentRecord, RESOURCE_AGENT_KEY, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::selector::format_terminal_id;

/// The `event` name of a [`ControllerUpdate`] line.
pub const CONTROLLER_EVENT: &str = "controller_changed";

/// One observed change to a Terminal's `phux.controller/v1` record: the
/// connection that last typed into it changed, or left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerUpdate {
    /// The Terminal whose controller changed.
    pub terminal: ResourceId,
    /// The record as the server wrote it, or `None` once that connection
    /// closed.
    pub controller: Option<Value>,
}

impl ControllerUpdate {
    /// The `controller_changed` line, shared by the CLI and MCP.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "event": CONTROLLER_EVENT,
            "terminal": format_terminal_id(&self.terminal),
            "controller": self.controller,
        })
    }
}

/// Subscribe `conn` to the records a watch follows for `terminal`.
pub(super) async fn subscribe_records(
    conn: &mut Connection,
    terminal: &ResourceId,
) -> Result<(), AttachError> {
    for key in [RESOURCE_AGENT_KEY, RESOURCE_CONTROLLER_KEY] {
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Resource(terminal.clone()),
            key: key.to_owned(),
        })
        .await?;
    }
    Ok(())
}

/// Fold one `METADATA_CHANGED` into a [`ControllerUpdate`] when it is a
/// Terminal-scoped `phux.controller/v1` record.
pub(super) fn controller_update(
    scope: &Scope,
    key: &str,
    value: Option<&[u8]>,
) -> Option<ControllerUpdate> {
    let Scope::Resource(id) = scope else {
        return None;
    };
    (key == RESOURCE_CONTROLLER_KEY).then(|| ControllerUpdate {
        terminal: id.clone(),
        controller: value.and_then(|bytes| serde_json::from_slice(bytes).ok()),
    })
}

/// Fold one `METADATA_CHANGED` into an [`AgentStateUpdate`] when it is a
/// Terminal-scoped `phux.agent/v1` record for a `tracked` Terminal,
/// remembering it in `last_seen` so the next update carries `previous`.
pub(super) fn agent_update(
    last_seen: &mut HashMap<ResourceId, AgentRecord>,
    scope: &Scope,
    key: &str,
    value: Option<&[u8]>,
    tracked: impl FnOnce(&ResourceId) -> bool,
) -> Option<AgentStateUpdate> {
    if key != RESOURCE_AGENT_KEY {
        return None;
    }
    let Scope::Resource(id) = scope else {
        return None;
    };
    if !tracked(id) {
        return None;
    }
    let record = value.and_then(parse_agent_record);
    let previous = match &record {
        Some(new) => last_seen.insert(id.clone(), new.clone()),
        None => last_seen.remove(id),
    };
    Some(AgentStateUpdate {
        terminal: Some(id.clone()),
        record,
        previous,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_controller_record_folds_into_its_line_and_a_delete_into_null() {
        let scope = Scope::Resource(ResourceId::local(7));
        let record =
            br#"{"client":3,"client_name":"phux-mobile","route":"bearer-quic","remote":true}"#;
        let update = controller_update(&scope, RESOURCE_CONTROLLER_KEY, Some(record));
        let line = update.map(|update| update.to_json()).unwrap_or_default();
        assert_eq!(line["event"], "controller_changed");
        assert_eq!(line["terminal"], "@7");
        assert_eq!(line["controller"]["remote"], true);

        let cleared = controller_update(&scope, RESOURCE_CONTROLLER_KEY, None);
        assert_eq!(cleared.map(|update| update.controller), Some(None));
        assert_eq!(
            controller_update(&scope, RESOURCE_AGENT_KEY, Some(record)),
            None
        );
        assert_eq!(
            controller_update(&Scope::Global, RESOURCE_CONTROLLER_KEY, None),
            None
        );
    }
}
