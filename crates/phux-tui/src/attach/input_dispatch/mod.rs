//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `ArrangementDoc`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_RESOURCE` reply.

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
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_actions;
#[cfg(test)]
mod tests_events;
#[cfg(test)]
mod tests_plugin_actions;

pub(super) use ctx::{DispatchCtx, DragGrab};
pub(super) use deferred::{DeferredArrangementEdit, apply_deferred_arrangement_edits};
pub(super) use dispatch::{dispatch_input_events, predict_now_ms, sync_overlays_to_focused_pane};
pub use effects::ReattachTarget;
pub(super) use effects::{PendingSessionRename, encode_layout_or_log};
/// The session picker's rows and its live-refresh key, so the
/// driver can rebuild an open picker when a fresh host inventory lands.
pub(super) use pickers::{SESSION_PICKER_LIVE_KEY, session_picker_rows};

pub(super) use dispatch::{send_replay_frames, terminal_in_alt_screen};
