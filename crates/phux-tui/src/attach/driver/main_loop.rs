//! The attach session's entry point and its frame-coalescing policy.
//!
//! [`main_loop`] builds the session state ([`SessionLoop`]), replays the
//! `ATTACHED` bootstrap through it, and then turns the crank: one
//! [`SessionLoop::step`] per wake-up until the session detaches or switches.

use std::collections::HashSet;

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::FrameKind;

use crate::attach::connection::Connection;
use crate::attach::exec_widgets::spawn_exec_feed_runners;
use crate::attach::outcome::AttachError;
use crate::predict::PredictiveConfig;
use crate::render::chrome::status_bar::Notice;

use super::entry::LoopExit;
use super::loop_state::{SessionLoop, Step};

/// phux-jhv8: upper bound on how many already-queued frames one `recv`
/// wake-up drains before painting. A back-to-back output burst (nvim
/// startup) is a few dozen frames; the cap only guards against a server
/// that streams without pause starving the stdin/signal `select!` arms.
pub(super) const FRAME_COALESCE_CAP: usize = 1024;

/// The terminal a frame would repaint under normal handling, if any — the
/// `vt_write` + render pair a coalesced burst can defer to a later same-pane
/// frame (phux-jhv8). Output and snapshot frames carry pane content; every
/// other frame (layout, lifecycle, control) paints through its own path or
/// not at all, so it never defers (returns `None`).
pub(super) const fn frame_paint_target(frame: &FrameKind) -> Option<&TerminalId> {
    match frame {
        FrameKind::TerminalOutput { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// Per-frame paint-deferral mask for a coalesced burst (phux-jhv8).
///
/// `targets[i]` is the pane frame `i` would repaint (`None` for control
/// frames). The result is `true` at `i` iff some later frame repaints the
/// *same* pane — meaning frame `i`'s paint is redundant and can be skipped
/// (its `vt_write` still applies). Each pane's LAST frame is therefore never
/// deferred, so every touched pane settles exactly once and none is left
/// stale; control frames (`None`) never defer.
pub(super) fn coalesce_defer_flags<T>(
    items: &[T],
    target: impl for<'a> Fn(&'a T) -> Option<&'a TerminalId>,
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

/// Drive the `tokio::select!` loop until detach or a session switch.
///
/// `initial_attached` is the `FrameKind::Attached` frame that
/// [`wait_for_attached`] already pulled off the wire; we replay it
/// through `handle_server_frame` so the focused-pane bookkeeping lives
/// in one place. Subsequent bootstrap and `TERMINAL_OUTPUT` frames come off the
/// wire as usual.
///
/// phux-eb0: returns a [`LoopExit`] so the outer loop in
/// [`run_with_stdout_predict`] can re-attach to another session without
/// dropping the transport or leaving raw mode. Every session-scoped local
/// in [`SessionLoop`] is rebuilt on each entry, so a re-attach starts from a
/// clean slate (no stale pane mirror, no carried-over predict queue).
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
    initial_attached: FrameKind,
    predict_cfg: PredictiveConfig,
    out: &mut W,
    // phux-fysb: the off-loop StdoutSink's backpressure flag. When the writer
    // drops a stale backlog under a slow terminal it sets this; we repaint the
    // latest state from scratch (a self-contained full frame supersedes the
    // dropped diffs). `None` for the synchronous test sink.
    needs_resync: Option<&std::sync::atomic::AtomicBool>,
    // Whether this connection negotiated `OutputMode::StateSync`. Gates the
    // per-frame `FRAME_ACK`: only a state-sync consumer's acks are tracked
    // server-side, so a raw consumer skips them (see `should_emit_frame_ack`).
    wants_state_sync: bool,
    // First-use moment consumed by this loop entry. Session switches receive
    // `None`, so they never repeat attach guidance.
    onboarding_claim: Option<crate::attach::onboarding::AttachClaim>,
    // phux-i0e8.2.3: transient status-bar notice to seed at attach time —
    // the reconnect loop's "re-attached after server restart". Applied to
    // the painter right after the bootstrap chrome refresh, so the first
    // bar paint (driven by the initial TERMINAL_SNAPSHOT burst) shows it;
    // expiry rides the ordinary 1 s status_tick. `None` on a first attach
    // and on session switches.
    initial_notice: Option<Notice>,
    // phux-foz.8: window index to select once this session's persisted
    // layout loads. Set by the outer loop when a one-step cross-session
    // window pick (`switch-session { name, window }`) drove the re-attach;
    // `None` on a plain attach/switch. Resolved (and consumed) on the
    // first layout reconcile; out-of-range degrades to the session's own
    // restored focus with a warning.
    initial_window: Option<usize>,
    // phux-jpqd: DFS leaf ordinal to focus (within `initial_window`) once
    // this session's layout loads — the pane half of a one-step
    // cross-session PANE pick (`switch-session { name, window, pane }`,
    // the agent-fleet foreign rows). `None` on a plain switch or a
    // window-only pick; resolved alongside `initial_window` and, like it,
    // degrades to a logged no-op if out of range.
    initial_pane: Option<usize>,
    // The window sidebar's on/off state carried in from the previous
    // `main_loop` entry when a `switch-session` drove this one. `None` on the
    // first attach — `[sidebar] enabled` seeds it; `Some(v)` on every
    // in-process switch, so a `toggle-sidebar` the user made survives moving
    // between spaces. Only the toggle is carried: the strip's width and edge
    // stay pure config, re-derived per entry.
    carried_sidebar_enabled: Option<bool>,
    // ADR-0053: the acknowledged-input replay journal, shared across attach
    // attempts by the CLI's reconnect loop (remote dials only — `None` on
    // UDS). The session loop re-decides every queued operation against this
    // connection's incarnation at bootstrap and replays the survivors; the
    // paste path in `dispatch_input_events` feeds it and the `COMMAND_RESULT`
    // intercept in the recv arm resolves it.
    input_replay: Option<
        std::rc::Rc<std::cell::RefCell<crate::attach::input_replay::InputReplayJournal>>,
    >,
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
        carried_sidebar_enabled,
    )?;
    // phux-r82.6: spawn one bounded interval runner per `exec` widget. The
    // runners execute off-loop and write into the widgets' shared caches;
    // the bar's normal repaint tick picks changed cells up, so the render
    // loop never blocks on a widget command. The guard aborts the tasks
    // (and via kill_on_drop, their children) when this attach loop ends.
    session.set_input_replay(input_replay);
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

    #[test]
    fn coalesce_defers_every_pane_frame_but_its_last() {
        // phux-jhv8: in a coalesced burst, every output frame for a pane
        // defers EXCEPT that pane's final frame, which settles the screen.
        let p = |id| Some(TerminalId::Local { id });
        // Single-pane burst: only the last frame paints.
        assert_eq!(
            coalesce_defer_flags(&[p(2), p(2), p(2)], Option::as_ref),
            vec![true, true, false]
        );
        // A lone frame never defers (preserves the one-frame-one-paint path).
        assert_eq!(coalesce_defer_flags(&[p(2)], Option::as_ref), vec![false]);
    }

    #[test]
    fn coalesce_keys_deferral_per_pane_not_globally() {
        // Two panes interleaved: each pane's LAST frame paints, so neither is
        // left stale even when the burst ends on the other pane's output.
        let p = |id| Some(TerminalId::Local { id });
        // A(defer, later A) B(defer, later B) A(last A) B(last B)
        assert_eq!(
            coalesce_defer_flags(&[p(1), p(2), p(1), p(2)], Option::as_ref),
            vec![true, true, false, false]
        );
        // Burst ending on a non-focused pane B must still paint A's last frame.
        assert_eq!(
            coalesce_defer_flags(&[p(1), p(1), p(2)], Option::as_ref),
            vec![true, false, false]
        );
    }

    #[test]
    fn output_honors_coalescing_decision() {
        let output = FrameKind::TerminalOutput {
            terminal_id: TerminalId::Local { id: 1 },
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: bytes::Bytes::new(),
        };
        assert!(frame_defers_paint(true, &output));
        assert!(!frame_defers_paint(false, &output));
    }

    #[test]
    fn coalesce_control_frames_never_defer() {
        // `None` (a non-painting control frame) never defers, and never
        // counts as a later same-pane paint for the frames before it.
        let p = |id| Some(TerminalId::Local { id });
        assert_eq!(
            coalesce_defer_flags(&[p(1), None, p(1)], Option::as_ref),
            vec![true, false, false]
        );
        assert_eq!(
            coalesce_defer_flags(&[None, None], Option::as_ref),
            vec![false, false]
        );
        let empty: [Option<TerminalId>; 0] = [];
        assert_eq!(
            coalesce_defer_flags(&empty, Option::as_ref),
            Vec::<bool>::new()
        );
    }
}
