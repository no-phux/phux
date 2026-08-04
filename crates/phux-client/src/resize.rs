//! Headless per-Terminal resize — set a pane's grid without a TTY.
//!
//! Every other way to size a pane goes through a *viewport*: a client
//! attaches, reports its outer geometry, and the server's window-size policy
//! folds that report into the Terminal's authoritative `(cols, rows)`. That
//! is exactly what a headless caller cannot do — it has no TTY to measure,
//! so the viewport it would contribute is the 80x24 no-TTY fallback. The
//! result was that an agent could read, drive, and record a pane but not
//! *size* one, and the only workaround was to stand up a real PTY and a real
//! `phux attach` purely for its side effect on the grid.
//!
//! This module closes that with the frame the wire already had:
//! [`FrameKind::TerminalResize`] (`TERMINAL_RESIZE`, `L1.md` §3.1) is a
//! C→S frame naming one Terminal and its exact cell dimensions, and the
//! reference server has driven `TIOCSWINSZ` from it since it landed. No
//! viewport, no attach, no subscription, no wire change.
//!
//! # Why this verifies instead of assuming
//!
//! `TERMINAL_RESIZE` is deliberately unacknowledged — the S→C
//! `TERMINAL_RESIZED` counterpart at `0x92` is spec-only — so a caller that
//! merely sends it learns nothing about whether the size took. That matters
//! here because the size a pane ends up at is *not* purely a function of the
//! request: under the view-derived `defaults.window-size` policies
//! (`smallest`, `largest`, `latest`) an attached client's viewport is folded
//! back in on the next attach, detach, or `SIGWINCH`, and under `manual` it
//! never is. So [`resize_to`] sends the frame and then reads the server's
//! own answer back with `GET_STATE` on the same connection, reporting the
//! geometry the server actually holds. A caller that gets an [`Ok`] and
//! [`ResizeOutcome::held`] knows the grid moved; one that gets an `Ok` and
//! `!held` knows something else owns the size. Neither can mistake a
//! delivered frame for an applied resize.
//!
//! The read-back is ordered, not racy: the server handles frames from one
//! connection in arrival order, and its `TERMINAL_RESIZE` handler updates
//! the registry `dims` synchronously — the same field `GET_STATE` projects
//! into [`TerminalInfo`] — before the next frame is read. (A `GET_SCREEN`
//! read-back would *not* be safe: that projection is served by the pane
//! actor from a different mailbox than the resize, and the actor's `select!`
//! polls the screen arm first.)

use std::num::NonZeroU16;
use std::path::Path;

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::TerminalInfo;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::get_state_on;

/// What a resize asked for, and what the server holds afterwards.
///
/// `applied` is read back from the server rather than echoed from the
/// request, so it is the geometry a subsequent `phux snapshot` will report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeOutcome {
    /// The `(cols, rows)` the caller asked for.
    pub requested: (u16, u16),
    /// The `(cols, rows)` the server reports for the pane after the resize.
    pub applied: (u16, u16),
}

impl ResizeOutcome {
    /// Whether the server settled on exactly the requested geometry.
    ///
    /// `false` means something else owns this Terminal's size — in practice
    /// an attached client's viewport under a view-derived
    /// `defaults.window-size` policy. It is the caller's cue to report a
    /// failure rather than to retry: retrying loses to the same policy.
    #[must_use]
    pub const fn held(self) -> bool {
        self.requested.0 == self.applied.0 && self.requested.1 == self.applied.1
    }
}

/// Resize `pane` to `cols` x `rows` and report the geometry the server
/// holds afterwards.
///
/// Opens a fresh connection, sends one `TERMINAL_RESIZE`, then reads the
/// pane's dimensions back out of a `GET_STATE` snapshot on that same
/// connection. No `HELLO`, no `ATTACH`, no subscription: this connection
/// never becomes a view of the pane, so it contributes nothing to the
/// window-size policy and cannot itself shrink what it just sized.
///
/// The dimensions are [`NonZeroU16`] because a zero-dimension grid does not
/// exist: libghostty rejects it and the server clamps it to one cell. Taking
/// the constraint in the type keeps the "0 is meaningless here" rule off
/// every call site's checklist.
///
/// # Errors
///
/// Returns [`AttachError::Io`] when the socket cannot be reached,
/// [`AttachError::Disconnected`] when the server closes mid-request, and
/// [`AttachError::Refused`] when the pane is absent from the post-resize
/// snapshot — which is how an unknown or just-died Terminal surfaces, since
/// `TERMINAL_RESIZE` itself has no error reply.
pub async fn resize_to(
    socket: &Path,
    pane: &TerminalId,
    cols: NonZeroU16,
    rows: NonZeroU16,
) -> Result<ResizeOutcome, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    conn.send(&FrameKind::TerminalResize {
        terminal_id: pane.clone(),
        cols: cols.get(),
        rows: rows.get(),
        pixel_width: None,
        pixel_height: None,
    })
    .await?;
    // Ordered behind the frame above on this connection; see the module
    // docs for why the registry read is the sound one and the actor's
    // screen projection is not.
    let (snapshot, degradation) = get_state_on(&mut conn).await?.into_parts();
    let applied = snapshot
        .panes
        .iter()
        .find(|info| info.id == *pane)
        .map(|info: &TerminalInfo| (info.cols, info.rows))
        .ok_or_else(|| {
            // The read-back searches `panes`, which is exactly the list a
            // hub's federation merge leaves incomplete. Absent-and-complete
            // means the Terminal is gone; absent-and-degraded means we could
            // not look where it lives, and saying the first when we mean the
            // second would report a healthy satellite pane as dead.
            if degradation.is_complete() {
                AttachError::Refused(format!(
                    "pane {pane:?} is not in the server's state after the resize"
                ))
            } else {
                AttachError::Refused(format!(
                    "pane {pane:?} was not visible after the resize, and this \
                     server's view of the fleet is incomplete ({}); the resize \
                     may have applied",
                    degradation.notices().join("; ")
                ))
            }
        })?;
    Ok(ResizeOutcome {
        requested: (cols.get(), rows.get()),
        applied,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests"
    )]

    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{SessionSnapshot, TerminalInfo};
    use tokio::net::UnixListener;

    use crate::testkit::{ScriptSpec, ScriptedServer};

    use super::*;

    fn nz(n: u16) -> NonZeroU16 {
        NonZeroU16::new(n).expect("nonzero literal")
    }

    /// A `GET_STATE` snapshot with one pane at `(cols, rows)`.
    fn snapshot_with(pane: &TerminalId, cols: u16, rows: u16) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), pane.clone()).with_panes(vec![
            TerminalInfo::new(pane.clone(), WindowId::new(1), cols, rows),
        ])
    }

    /// Drive `resize_to` against the shared scripted server and return
    /// `(outcome, frames the client actually sent)`.
    async fn drive(
        state: SessionSnapshot,
        pane: &TerminalId,
    ) -> (Result<ResizeOutcome, AttachError>, Vec<FrameKind>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move {
            ScriptedServer::accept(&listener, ScriptSpec::new().state(state)).await
        });
        let outcome = resize_to(&socket, pane, nz(120), nz(40)).await;
        let seen = server.await.expect("scripted server task");
        (outcome, seen)
    }

    #[tokio::test]
    async fn sends_only_terminal_resize_and_a_read_back() {
        let pane = TerminalId::local(7);
        let (outcome, seen) = drive(snapshot_with(&pane, 120, 40), &pane).await;

        let outcome = outcome.expect("the scripted server answers GET_STATE");
        assert_eq!(outcome.applied, (120, 40));
        assert!(outcome.held());

        // The guarantee that makes this verb usable against a session
        // someone is working in: no ATTACH and no VIEWPORT_RESIZE, so this
        // connection never becomes a view and never contributes its 80x24
        // no-TTY viewport to the window-size policy. That is the same
        // regression `phux rec` had to be built around.
        assert!(
            !seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Attach { .. } | FrameKind::ViewportResize { .. }
            )),
            "resize must not attach or report a viewport; sent {seen:?}"
        );
        assert!(
            matches!(
                seen.first(),
                Some(FrameKind::TerminalResize {
                    cols: 120,
                    rows: 40,
                    ..
                })
            ),
            "the resize must be the first frame on the wire; sent {seen:?}"
        );
        assert_eq!(seen.len(), 2, "resize + read-back, nothing else: {seen:?}");
    }

    #[tokio::test]
    async fn reports_the_servers_size_when_it_differs_from_the_request() {
        // The case the whole read-back exists for: an attached view under a
        // view-derived `window-size` policy holds the pane somewhere else.
        // `resize_to` must surface the server's number, not echo the
        // request back and let the caller believe it won.
        let pane = TerminalId::local(7);
        let (outcome, _) = drive(snapshot_with(&pane, 80, 24), &pane).await;

        let outcome = outcome.expect("the scripted server answers GET_STATE");
        assert_eq!(outcome.requested, (120, 40));
        assert_eq!(outcome.applied, (80, 24));
        assert!(!outcome.held());
    }

    #[tokio::test]
    async fn a_pane_missing_from_the_read_back_is_a_refusal() {
        // `TERMINAL_RESIZE` has no error reply, so an unknown or just-died
        // pane can only be detected by its absence downstream. Silently
        // reporting success there would be the exact failure mode the
        // read-back exists to prevent.
        let pane = TerminalId::local(7);
        let other = TerminalId::local(9);
        let (outcome, _) = drive(snapshot_with(&other, 120, 40), &pane).await;

        assert!(
            matches!(outcome, Err(AttachError::Refused(_))),
            "expected a refusal, got {outcome:?}"
        );
    }

    #[test]
    fn held_is_exact_match_on_both_axes() {
        let exact = ResizeOutcome {
            requested: (120, 40),
            applied: (120, 40),
        };
        assert!(exact.held());

        // A policy that clamps only one axis still means the caller did not
        // get what it asked for; `held` must not be a per-axis "close
        // enough".
        let one_axis = ResizeOutcome {
            requested: (120, 40),
            applied: (120, 24),
        };
        assert!(!one_axis.held());
    }

    #[test]
    fn zero_dimensions_are_unrepresentable() {
        // The guardrail this module leans on: there is no way to spell a
        // zero-column resize at the call site, so no caller has to remember
        // not to.
        assert!(NonZeroU16::new(0).is_none());
        assert_eq!(nz(1).get(), 1);
    }
}
