//! The attach session's entry point and its frame-coalescing policy.
//!
//! [`main_loop`] builds the session state ([`SessionLoop`]), replays the
//! `ATTACHED` bootstrap through it, and then turns the crank: one
//! [`SessionLoop::step`] per wake-up until the session detaches or switches.

use std::collections::HashSet;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::FrameKind;

use crate::attach::connection::Connection;
use crate::attach::exec_widgets::spawn_exec_feed_runners;
use crate::attach::outcome::AttachError;
use crate::predict::PredictiveConfig;
use crate::render::chrome::status_bar::Notice;

use super::entry::LoopExit;
use super::loop_state::{SessionLoop, Step};

/// How many queued frames one `recv` wake-up drains before painting; only
/// guards against a server that never pauses starving the other arms.
pub(super) const FRAME_COALESCE_CAP: usize = 1024;

/// The pane a frame would repaint (output and snapshot frames); other frames
/// never defer.
pub(super) const fn frame_paint_target(frame: &FrameKind) -> Option<&ResourceId> {
    match frame {
        FrameKind::ResourceOutput { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// Per-frame paint-deferral mask: `true` at `i` iff a later frame repaints
/// the same pane, so each pane paints once, on its last frame.
pub(super) fn coalesce_defer_flags<T>(
    items: &[T],
    target: impl for<'a> Fn(&'a T) -> Option<&'a ResourceId>,
) -> Vec<bool> {
    let paint_count = items.iter().filter(|item| target(item).is_some()).count();
    let mut seen = HashSet::with_capacity(paint_count);
    let mut deferred = Vec::with_capacity(items.len());
    for item in items.iter().rev() {
        deferred.push(target(item).is_some_and(|pane| !seen.insert(pane)));
    }
    deferred.reverse();
    deferred
}

/// Apply the per-pane last-wins coalescing decision.
pub(super) const fn frame_defers_paint(deferred_by_coalesce: bool, _frame: &FrameKind) -> bool {
    deferred_by_coalesce
}

/// Drive the `select!` loop until detach or a session switch. The
/// `ATTACHED` frame [`wait_for_attached`] pulled is replayed through
/// `handle_server_frame`; every session-scoped local is rebuilt per entry, so
/// the outer loop can re-attach without leaving raw mode.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "per-entry knobs from attach_session's outer loop (foz-6 onboarding + foz-8 window pick + jpqd cross-session pane pick); the list is the call contract with `entry.rs`, and the driver folds it into `SessionLoop` on the first statement"
)]
pub(super) async fn main_loop<W: crate::attach::RenderSink>(
    conn: &mut Connection,
    control_dial: &crate::attach::Dial,
    initial_attached: FrameKind,
    predict_cfg: PredictiveConfig,
    out: &mut W,
    // The stdout writer's backpressure flag (`None` for the test sink).
    needs_resync: Option<&std::sync::atomic::AtomicBool>,
    // Whether this connection negotiated `OutputMode::StateSync`. Gates the
    // per-frame `FRAME_ACK`: only a state-sync consumer's acks are tracked
    // server-side, so a raw consumer skips them (see `should_emit_frame_ack`).
    wants_state_sync: bool,
    // First-use moment consumed by this loop entry. Session switches receive
    // `None`, so they never repeat attach guidance.
    onboarding_claim: Option<crate::attach::onboarding::AttachClaim>,
    // Attach-time notice (reconnects only; not on session switches).
    initial_notice: Option<Notice>,
    // A one-step cross-session pick's window index, resolved on the first
    // layout reconcile.
    initial_window: Option<usize>,
    // And its pane (DFS leaf ordinal within that window).
    initial_pane: Option<usize>,
    // Authoritative ResourceId to focus after re-attach, from
    // a graph-discovered agent row (`switch-session { resource }`). Wins
    // over window/pane indices and works before a TUI layout exists.
    initial_resource: Option<ResourceId>,
    // Sidebar state carried from the previous entry (`None` on first attach).
    carried_sidebar: Option<super::entry::CarriedSidebar>,
    // ADR-0053 replay journal (remote dials only).
    input_replay: Option<
        std::rc::Rc<std::cell::RefCell<crate::attach::input_replay::InputReplayJournal>>,
    >,
    // The stray satellite panes an earlier entry on this
    // connection still owed a kill. Empty on the first attach.
    orphan_kills: super::orphans::OrphanKills,
    // Per-identity review status carried across session switches
    // on this connection. Empty on the first attach.
    review: crate::attach::review::ReviewIndex,
) -> Result<LoopExit, AttachError> {
    let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
        AttachError::Protocol("attach loop started before bootstrap negotiation".to_owned())
    })?;
    let mut session = SessionLoop::new(
        negotiated,
        predict_cfg,
        wants_state_sync,
        onboarding_claim,
        initial_window,
        initial_pane,
        initial_resource,
        carried_sidebar,
    )?;
    session.set_control_dial(control_dial.clone());
    // One bounded runner per `exec` widget, off-loop; the guard aborts them
    // (and their children) when the loop ends.
    session.set_input_replay(input_replay);
    session.set_orphan_kills(orphan_kills);
    session.set_review(review);
    let _exec_runners = spawn_exec_feed_runners(session.exec_feeds());
    if let Some(exit) = session
        .bootstrap(conn, out, initial_attached, initial_notice)
        .await?
    {
        return Ok(exit);
    }
    loop {
        match session.step(conn, out, needs_resync).await? {
            Step::Continue => {}
            Step::Exit(exit) => return Ok(exit),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// Each pane paints once, on its own last frame in the burst (so a burst
    /// ending on another pane leaves none stale); control frames never defer
    /// or count as a later paint.
    #[test]
    fn coalesce_defers_every_pane_frame_but_its_last() {
        let p = |id| Some(ResourceId::Local { id });
        for (targets, flags) in [
            (vec![p(2), p(2), p(2)], vec![true, true, false]),
            (vec![p(2)], vec![false]),
            (vec![p(1), p(2), p(1), p(2)], vec![true, true, false, false]),
            (vec![p(1), p(1), p(2)], vec![true, false, false]),
            (vec![p(1), None, p(1)], vec![true, false, false]),
            (vec![None, None], vec![false, false]),
            (vec![], vec![]),
        ] {
            assert_eq!(
                coalesce_defer_flags(&targets, Option::as_ref),
                flags,
                "{targets:?}"
            );
        }
        let output = FrameKind::ResourceOutput {
            terminal_id: ResourceId::Local { id: 1 },
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: bytes::Bytes::new(),
        };
        assert!(frame_defers_paint(true, &output));
        assert!(!frame_defers_paint(false, &output));
    }
}
