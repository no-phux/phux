//! The `LoopExit` vocabulary `main_loop` hands back to the outer attach
//! loop in `entry`, and the state one loop entry carries into the next
//! across a session switch.

use phux_protocol::ids::ResourceId;

use crate::attach::input_dispatch::ReattachTarget;
use crate::attach::outcome::AttachEnd;
use crate::attach::paint::StatusBarPaint;
use crate::render::chrome::status_bar::StatusBarPainter;

/// How a `main_loop` entry ended: `Detached` (the process then exits via
/// `exit_after_detach`) or `SwitchTo` (the outer loop re-handshakes and
/// re-enters with fresh session state).
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "SwitchTo carries the full re-attach request including resource identity"
)]
pub(super) enum LoopExit {
    /// The session ended; `end` says why, for the cooked-terminal message.
    Detached {
        end: AttachEnd,
        locally_requested: bool,
    },
    /// Re-attach on the same connection — to an existing session or a
    /// newly-created one.
    SwitchTo {
        /// Where to re-attach.
        target: ReattachTarget,
        /// The sidebar's runtime state, so a switch neither reverts a
        /// `toggle-sidebar` nor snaps back a dragged width.
        sidebar: CarriedSidebar,
        /// Stray satellite panes still owed a kill (connection state).
        orphan_kills: super::orphans::OrphanKills,
        /// Per-identity review status for this connection. Session
        /// loops rebuild pane slots; this does not.
        review: crate::attach::review::ReviewIndex,
        /// Server-wide peer caches, so the next entry's sidebar paints them
        /// on its first frame.
        peers: super::loop_state::CarriedPeers,
    },
}

/// Classify a detach independently from the wire frame that completed it.
/// A server `DETACHED` is local only when this client had already requested
/// detach; pane death remains its own ending even if the events race.
pub(super) const fn is_local_detach(end: AttachEnd, local_intent: bool) -> bool {
    local_intent && matches!(end, AttachEnd::Detached { .. })
}

pub(super) const fn detached_loop_exit(end: AttachEnd, local_intent: bool) -> LoopExit {
    LoopExit::Detached {
        end,
        locally_requested: is_local_detach(end, local_intent),
    }
}

/// A one-step cross-session pick (`switch-session` naming a window, pane, or
/// resource) that the next loop entry focuses once its layout lands.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct EntryPick {
    /// The window index to select.
    pub(super) window: Option<usize>,
    /// The pane within it: its DFS leaf ordinal.
    pub(super) pane: Option<usize>,
    /// The authoritative resource to focus, from a graph-discovered agent
    /// row. Wins over the indices and works before a TUI layout exists.
    pub(super) resource: Option<ResourceId>,
}

/// What one `main_loop` entry hands the next across a session switch.
pub(super) struct SwitchCarry {
    /// The sidebar's runtime state.
    pub sidebar: CarriedSidebar,
    /// Server-wide peer caches, so the first frame paints them.
    pub peers: super::loop_state::CarriedPeers,
}

/// Sidebar state carried across an in-process session switch; runtime-only
/// (ADR-0101).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CarriedSidebar {
    /// `toggle-sidebar`'s current state.
    pub enabled: bool,
    /// The strip width when a drag moved it away from `[sidebar] width`;
    /// `None` lets the next entry's config load decide, as before.
    pub width: Option<u16>,
}

/// The sidebar's enabled flag at `main_loop` entry: config decides the first
/// attach; after that the carried runtime value wins in both directions.
pub(super) const fn seed_sidebar_enabled(carried: Option<bool>, configured: bool) -> bool {
    match carried {
        Some(enabled) => enabled,
        None => configured,
    }
}

pub(super) fn finish_onboarding_claim(
    claim: Option<crate::attach::onboarding::AttachClaim>,
    delivery_accepted: bool,
) {
    if delivery_accepted && let Some(claim) = claim {
        let _ = claim.commit();
    }
}

pub(super) fn finish_return_onboarding_after_paint(
    claim: &mut Option<crate::attach::onboarding::AttachClaim>,
    status_bar: Option<&StatusBarPainter>,
    paint: StatusBarPaint,
) {
    if paint.delivered(status_bar, crate::attach::onboarding::RETURN_NOTICE) {
        finish_onboarding_claim(claim.take(), true);
    }
}
