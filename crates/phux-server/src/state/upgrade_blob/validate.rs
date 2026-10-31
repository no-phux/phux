//! Validating a blob before anything is rebuilt.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// Check the blob's topology is complete and internally consistent before
/// any mutation, so a bad handoff cannot leave a partial tree.
pub(crate) fn validate_upgrade_blob(blob: &StateBlob) -> Result<(), RebuildError> {
    let sessions = unique_wire_ids("session", blob.sessions.iter().map(|s| s.wire_id))?;
    let windows = unique_wire_ids("window", blob.windows.iter().map(|w| w.wire_id))?;
    let panes = unique_wire_ids("pane", blob.panes.iter().map(|p| p.wire_id))?;
    // Agent sessions share the pane wire-id space: one id, one resource.
    unique_wire_ids(
        "resource",
        blob.panes
            .iter()
            .map(|p| p.wire_id)
            .chain(blob.agent_sessions.iter().map(|a| a.wire_id)),
    )?;

    for s in &blob.sessions {
        require_refs("window", &s.window_wire_ids, &windows)?;
        if let Some(id) = s.active_window {
            require_ref("window", id, &windows)?;
        }
    }
    for w in &blob.windows {
        require_ref("session", w.session_wire_id, &sessions)?;
        require_refs("pane", &w.pane_wire_ids, &panes)?;
        if let Some(id) = w.active_resource {
            require_ref("pane", id, &panes)?;
        }
        if let Some(layout) = &w.layout {
            validate_layout(layout, &panes)?;
        }
    }
    for p in &blob.panes {
        require_ref("window", p.window_wire_id, &windows)?;
        match (p.master_fd, p.child_pid) {
            (Some(_), Some(_)) | (None, None) => {}
            _ => return Err(RebuildError::InvalidPtyPair { wire_id: p.wire_id }),
        }
    }
    Ok(())
}

pub(in crate::state) fn unique_wire_ids(
    kind: &'static str,
    ids: impl IntoIterator<Item = u32>,
) -> Result<HashSet<u32>, RebuildError> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(RebuildError::DuplicateId { kind, id });
        }
    }
    Ok(seen)
}

pub(in crate::state) fn require_ref(
    kind: &'static str,
    id: u32,
    known: &HashSet<u32>,
) -> Result<(), RebuildError> {
    if known.contains(&id) {
        Ok(())
    } else {
        Err(RebuildError::DanglingRef { kind, id })
    }
}

pub(in crate::state) fn require_refs(
    kind: &'static str,
    ids: &[u32],
    known: &HashSet<u32>,
) -> Result<(), RebuildError> {
    ids.iter().try_for_each(|id| require_ref(kind, *id, known))
}

pub(in crate::state) fn validate_layout(
    node: &LayoutBlob,
    panes: &HashSet<u32>,
) -> Result<(), RebuildError> {
    match node {
        LayoutBlob::Leaf(id) => require_ref("pane", *id, panes),
        LayoutBlob::Split { left, right, .. } => {
            validate_layout(left, panes)?;
            validate_layout(right, panes)
        }
    }
}
