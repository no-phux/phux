//! Correlation and input-safety state for the literal path insertion picker.

use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{ClientId, ResourceId, SatelliteHost};

use super::pane_state::{PaneSlot, pane_exited, pane_satellite_down};
use std::collections::HashMap;

/// A target is captured on opening, never inferred from focus at commit.
#[derive(Debug, Clone)]
pub(super) struct PendingPath {
    pub target: ResourceId,
    pub holder: Option<ClientId>,
    pub request_id: u32,
    pub root: String,
    pub query: String,
}

pub(super) const fn supported(features: ServerFeatureExtSet) -> bool {
    features.contains(ServerFeatureExt::PathQuery)
}

pub(super) fn may_insert(
    pending: &PendingPath,
    focused: Option<&ResourceId>,
    own: Option<ClientId>,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> bool {
    if focused != Some(&pending.target)
        || pane_exited(panes, &pending.target)
        || pane_satellite_down(panes, &pending.target)
    {
        return false;
    }
    let Some(slot) = panes.get(&pending.target) else {
        return false;
    };
    slot.input_holder == pending.holder
        && slot.input_holder.is_none_or(|holder| Some(holder) == own)
}

/// Conservative POSIX-shell single-quote encoding. Always quote: whitespace,
/// control bytes and shell metacharacters stay literal, with no submission.
pub(super) fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

pub(super) fn host(pending: &PendingPath) -> Option<SatelliteHost> {
    pending.target.host().cloned()
}

pub(super) fn reply_matches(pending: &PendingPath, id: u32, root: &str, query: &str) -> bool {
    id == pending.request_id && pending.root == root && pending.query == query
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_negotiated_path_query_extension_enables_picker() {
        assert!(!supported(ServerFeatureExtSet::new()));
        assert!(supported(ServerFeatureExtSet::with(&[
            ServerFeatureExt::PathQuery
        ])));
    }
    #[test]
    fn quoting_never_submits_or_evaluates_shell_syntax() {
        assert_eq!(shell_quote("/a b/it's $(evil)"), "'/a b/it'\\''s $(evil)'");
    }

    #[test]
    fn reply_id_and_query_must_both_match_latest_generation() {
        let pending = PendingPath {
            target: ResourceId::local(3),
            holder: None,
            request_id: 8,
            root: "/src".into(),
            query: "cargo".into(),
        };
        assert!(reply_matches(&pending, 8, "/src", "cargo"));
        assert!(!reply_matches(&pending, 7, "/src", "cargo"));
        assert!(!reply_matches(&pending, 8, "/src", "older"));
        assert!(!reply_matches(&pending, 8, "/elsewhere", "cargo"));
    }

    #[test]
    fn focus_or_input_lease_change_refuses_insertion() {
        let pane = ResourceId::local(3);
        let mut slot = PaneSlot::new().expect("pane");
        let mut pending = PendingPath {
            target: pane.clone(),
            holder: None,
            request_id: 8,
            root: "/".into(),
            query: String::new(),
        };
        let mut panes = HashMap::from([(pane.clone(), slot)]);
        assert!(may_insert(&pending, Some(&pane), None, &panes));
        assert!(!may_insert(
            &pending,
            Some(&ResourceId::local(4)),
            None,
            &panes
        ));
        slot = panes.remove(&pane).expect("pane");
        slot.input_holder = Some(ClientId::new(9));
        panes.insert(pane.clone(), slot);
        assert!(
            !may_insert(&pending, Some(&pane), Some(ClientId::new(9)), &panes),
            "the lease changed after opening"
        );
        pending.holder = Some(ClientId::new(9));
        assert!(
            !may_insert(&pending, Some(&pane), Some(ClientId::new(8)), &panes),
            "another client holds the lease"
        );
        assert!(may_insert(
            &pending,
            Some(&pane),
            Some(ClientId::new(9)),
            &panes
        ));
    }
}
