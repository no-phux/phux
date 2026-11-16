//! Spawn bookkeeping: which panes this client spawned and which spawns
//! are still unanswered.

use super::{HashMap, HashSet, ParkedAdopt, PendingSplit, PendingWindow, ResourceId};

/// The satellite panes a frame's parked windows and splits were spawned as;
/// each proves its satellite answered a relayed spawn.
pub(in super::super) fn spawned_satellite_panes(parked: &[ParkedAdopt]) -> Vec<ResourceId> {
    parked
        .iter()
        .filter_map(ParkedAdopt::pane)
        .cloned()
        .collect()
}

/// The panes this client spawned for windows/splits still awaiting attach.
pub(in super::super) fn parked_spawned_panes(
    windows: &HashMap<u32, PendingWindow>,
    splits: &HashMap<u32, PendingSplit>,
) -> Vec<crate::attach::actions::SpawnedPane> {
    let windows = windows.values().filter_map(PendingWindow::spawned_pane);
    let splits = splits.values().filter_map(PendingSplit::spawned_pane);
    windows.chain(splits).cloned().collect()
}

/// Request ids of windows and splits whose spawn has not answered yet.
pub(in super::super) fn unanswered_spawns(
    windows: &HashMap<u32, PendingWindow>,
    splits: &HashMap<u32, PendingSplit>,
) -> HashSet<u32> {
    let windows = windows
        .iter()
        .filter(|(_, window)| window.adopt.is_none())
        .map(|(id, _)| *id);
    let splits = splits
        .iter()
        .filter(|(_, split)| split.awaiting_spawn())
        .map(|(id, _)| *id);
    windows.chain(splits).collect()
}
