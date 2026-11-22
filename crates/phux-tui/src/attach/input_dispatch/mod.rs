//! Input dispatcher: parser events to wire frames or layout-action effects,
//! including the predictive-echo feed and the parked spawns that bridge a
//! `split-pane` / `new-window` to its `SPAWN_RESOURCE` reply.

pub(in crate::attach) mod approvals;
mod args;
mod chrome_drag;
mod ctx;
mod deferred;
mod dispatch;
mod effects;
// The handler modules the action registry's rows name (ADR-0172).
pub(super) mod organize;
mod pickers;
pub(super) mod run_action;
pub(super) mod session_management;
pub(super) mod tab_lock;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_actions;
#[cfg(test)]
mod tests_events;
#[cfg(test)]
mod tests_plugin_actions;
#[cfg(test)]
mod tests_scroll;
#[cfg(test)]
mod tests_slot_tabs;

pub(super) use ctx::{DispatchCtx, DragGrab};
pub(super) use deferred::{DeferredArrangementEdit, apply_deferred_arrangement_edits};
pub(super) use dispatch::{
    dispatch_input_events, predict_now_ms, sidebar_rows, sync_overlays_to_focused_pane,
};
pub use effects::ReattachTarget;
pub(super) use effects::{PendingSessionRename, encode_layout_or_log};
/// The session picker's rows and its live-refresh key, so the
/// driver can rebuild an open picker when a fresh host inventory lands.
pub(super) use pickers::{SESSION_PICKER_LIVE_KEY, WindowNames, session_picker_rows};
pub(super) use run_action::{PALETTE_LIVE_KEY, PaletteSources, palette_rows};

pub(super) use dispatch::{send_replay_frames, terminal_in_alt_screen};
