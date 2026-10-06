//! Headless per-Terminal resize: set a pane's grid without a TTY or a
//! viewport, via `RESIZE_TERMINAL` (`L1.md` §3.1).
//!
//! The frame is unacknowledged and a view-derived `window-size` policy may
//! override it, so [`resize_to`] reads the geometry back with `GET_STATE` on
//! the same connection. That read is ordered: the server applies the resize
//! to the registry before handling the next frame (a `GET_SCREEN` read-back
//! would race the pane actor).

use std::num::NonZeroU16;
use std::path::Path;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::ResourceInfo;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::get_state_on;

/// What a resize asked for, and what the server holds afterwards (read
/// back, not echoed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeOutcome {
    /// The `(cols, rows)` the caller asked for.
    pub requested: (u16, u16),
    /// The `(cols, rows)` the server reports for the pane after the resize.
    pub applied: (u16, u16),
}

impl ResizeOutcome {
    /// Whether the server settled on exactly the requested geometry; `false`
    /// means something else (a view-derived size policy) owns it.
    #[must_use]
    pub const fn held(self) -> bool {
        self.requested.0 == self.applied.0 && self.requested.1 == self.applied.1
    }
}

/// Resize `pane` to `cols` x `rows` and report the geometry the server
/// holds afterwards. Never attaches, so it is never a view of the pane.
///
/// # Errors
///
/// Transport failures, and [`AttachError::Refused`] when the pane is absent
/// from the read-back (how an unknown Terminal surfaces).
pub async fn resize_to(
    socket: &Path,
    pane: &ResourceId,
    cols: NonZeroU16,
    rows: NonZeroU16,
) -> Result<ResizeOutcome, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    conn.send(&FrameKind::ResizeTerminal {
        terminal_id: pane.clone(),
        cols: cols.get(),
        rows: rows.get(),
        cell_px: None,
    })
    .await?;
    let (snapshot, degradation) = get_state_on(&mut conn).await?.into_parts();
    drop(conn);
    let applied = snapshot
        .resources
        .iter()
        .find(|info| info.id == *pane)
        .map(|info: &ResourceInfo| (info.cols, info.rows))
        .ok_or_else(|| {
            // Absent from a degraded view is not proof the pane is gone.
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
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
    use tokio::net::UnixListener;

    use crate::testkit::{ScriptSpec, ScriptedServer};

    use super::*;

    fn nz(n: u16) -> NonZeroU16 {
        NonZeroU16::new(n).expect("nonzero literal")
    }

    /// A `GET_STATE` snapshot with one pane at `(cols, rows)`.
    fn snapshot_with(pane: &ResourceId, cols: u16, rows: u16) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), pane.clone()).with_resources(
            vec![ResourceInfo::new(
                pane.clone(),
                WindowId::new(1),
                cols,
                rows,
            )],
        )
    }

    /// Drive `resize_to` against the shared scripted server and return
    /// `(outcome, frames the client actually sent)`.
    async fn drive(
        state: SessionSnapshot,
        pane: &ResourceId,
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
    async fn negotiates_then_sends_terminal_resize_and_read_back() {
        let pane = ResourceId::local(7);
        let (outcome, seen) = drive(snapshot_with(&pane, 120, 40), &pane).await;

        let outcome = outcome.expect("the scripted server answers GET_STATE");
        assert_eq!(outcome.applied, (120, 40));
        assert!(outcome.held());

        // Never a view: no ATTACH and no VIEWPORT_RESIZE.
        assert!(
            !seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Attach { .. } | FrameKind::ViewportResize { .. }
            )),
            "resize must not attach or report a viewport; sent {seen:?}"
        );
        assert!(
            matches!(seen.first(), Some(FrameKind::Hello { .. })),
            "HELLO must be first; sent {seen:?}"
        );
        assert!(matches!(
            seen.get(1),
            Some(FrameKind::ResizeTerminal {
                cols: 120,
                rows: 40,
                ..
            })
        ));
        assert!(matches!(
            seen.get(2),
            Some(FrameKind::Command {
                command: phux_protocol::wire::frame::Command::GetState { .. },
                ..
            })
        ));
        assert_eq!(seen.len(), 3, "HELLO + resize + read-back: {seen:?}");
    }

    #[tokio::test]
    async fn reports_the_servers_size_when_it_differs_from_the_request() {
        // A view-derived size policy held the pane elsewhere.
        let pane = ResourceId::local(7);
        let (outcome, _) = drive(snapshot_with(&pane, 80, 24), &pane).await;

        let outcome = outcome.expect("the scripted server answers GET_STATE");
        assert_eq!(outcome.requested, (120, 40));
        assert_eq!(outcome.applied, (80, 24));
        assert!(!outcome.held());
    }

    #[tokio::test]
    async fn a_pane_missing_from_the_read_back_is_a_refusal() {
        // `RESIZE_TERMINAL` has no error reply; absence is the only signal.
        let pane = ResourceId::local(7);
        let other = ResourceId::local(9);
        let (outcome, _) = drive(snapshot_with(&other, 120, 40), &pane).await;

        assert!(
            matches!(outcome, Err(AttachError::Refused(_))),
            "expected a refusal, got {outcome:?}"
        );
    }
}
