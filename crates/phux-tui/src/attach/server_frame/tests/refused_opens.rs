//! A refused open of a satellite session's pane.

use super::*;

/// A refusal opens nothing, bells, names the host and session, and never
/// kills the session's own pane. An unreachable satellite opens it down
/// instead, so typing cannot land in the pane that was in view.
#[test]
fn a_refused_satellite_session_opens_nothing_or_opens_down() {
    for (frame, down) in [
        (unreachable_refusal(9), true),
        (
            FrameKind::Error {
                request_id: Some(9),
                code: ErrorCode::TerminalNotFound,
                message: "no such terminal".to_owned(),
            },
            false,
        ),
    ] {
        let (outcome, h) = adopt_reply_in(
            ArrangementDoc::single(tid(1)),
            Vec::new(),
            "edge/build",
            Adopt::existing(edge_pane()),
            frame,
        );
        let focus = if down { edge_pane() } else { tid(1) };
        assert_eq!(h.mirror.focused_resource, Some(focus));
        assert_eq!(h.mirror.workspace.groups.len(), 1 + usize::from(down));
        assert_eq!(outcome.emit_set_metadata, down);
        assert_eq!(outcome.layout_replaced, down);
        let slot = h.mirror.panes.get(&edge_pane());
        assert_eq!(slot.is_some_and(|slot| slot.satellite_down), down);
        assert!(h.belled());
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0].text.contains("edge/build"),
            "{}",
            outcome.notices[0].text
        );
        assert!(outcome.kill_orphans.is_empty());
        assert!(h.mirror.pending_windows.is_empty());
    }
}
