//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_TERMINAL` reply.

mod args;
mod ctx;
mod dispatch;
mod effects;
mod pickers;
mod run_action;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_actions;
#[cfg(test)]
mod tests_events;

pub(super) use ctx::{DispatchCtx, DragGrab};
pub(super) use dispatch::{dispatch_input_events, predict_now_ms, sync_overlays_to_focused_pane};
pub use effects::ReattachTarget;
pub(super) use effects::encode_layout_or_log;

/// Canonical names of every action `run_action` handles.
///
/// The list itself lives in [`phux_config::vocab`] (phux-i0e8.3.1) so
/// `phux config check` can validate against it; this re-export keeps the
/// dispatcher-side path working. The command-palette registry
/// ([`super::action_registry::REGISTRY`]) is checked against this list by
/// a unit test so the two cannot drift: adding a `run_action` arm without
/// adding it to the vocab (and to the registry) fails CI. Keep the vocab
/// list in sync with the `match resolved.action.as_str()` arms below —
/// they are the same set by construction, and the test enforces it.
pub use phux_config::vocab::ACTION_NAMES;

pub(super) use dispatch::terminal_in_alt_screen;
