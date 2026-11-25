//! Which fleet row waits on you, and the longest-waiting one the list
//! opens on.

use std::collections::{BTreeMap, HashMap};

use phux_client::agent_meta::{AgentAttention, AgentRecord};
use phux_config::keybind::ResolvedAction;
use phux_protocol::ResourceId;

use super::FleetPaneMeta;
use crate::layout::ArrangementDoc;

/// Whether a pane row needs you: it asked, or its agent declares high attention.
pub(super) fn waits(meta: &FleetPaneMeta, record: Option<&AgentRecord>) -> bool {
    meta.attention || record.is_some_and(|r| r.effective_attention() == AgentAttention::High)
}

/// `focus-pane { window, pane }`. Ordinals never approach `i64::MAX`.
pub(super) fn focus_pane(w: usize, p: usize) -> ResolvedAction {
    let int = |n: usize| toml::Value::Integer(i64::try_from(n).unwrap_or(i64::MAX));
    let args = BTreeMap::from([("window".to_owned(), int(w)), ("pane".to_owned(), int(p))]);
    crate::attach::resolved("focus-pane", args)
}

/// The attached session's pane that has waited longest, for the list to open
/// on: dated asks oldest first, then undated ones in layout order.
// ponytail: the date is when this client saw the ask, so panes asking before
// attach tie; a server-sent waiting time would order them.
pub(in crate::attach) fn oldest_waiting(
    workspace: &ArrangementDoc,
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    pane_meta: &HashMap<ResourceId, FleetPaneMeta>,
) -> Option<ResolvedAction> {
    let blank = FleetPaneMeta::default();
    workspace
        .groups
        .iter()
        .enumerate()
        .flat_map(|(w, window)| {
            let tree = window.state.tree.as_ref();
            let leaves = tree.map(crate::layout::leaves).unwrap_or_default();
            leaves
                .into_iter()
                .enumerate()
                .map(move |(p, id)| (w, p, id))
        })
        .filter_map(|(w, p, id)| {
            let meta = pane_meta.get(&id).unwrap_or(&blank);
            let key = (meta.asked_at.is_none(), meta.asked_at);
            waits(meta, agent_meta.get(&id)).then_some((key, w, p))
        })
        .min_by_key(|(key, ..)| *key)
        .map(|(_, w, p)| focus_pane(w, p))
}
