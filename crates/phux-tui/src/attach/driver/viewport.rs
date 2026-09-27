//! Outer-terminal viewport reads, the SIGWINCH frame builder, and the
//! per-leaf PTY reflow emitters.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::os::fd::AsFd;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, ViewportInfo};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::layout::{LayoutState, Workspace};

/// The zoom- and sidebar-honoring per-leaf rects (empty without a seeded
/// tree), via the paint path's memoized tiling since it runs every batch.
pub(super) fn view_rects(
    workspace: &Workspace,
    zoomed: Option<&ResourceId>,
    content: crate::layout::Rect,
    viewport_dims: (u16, u16),
) -> HashMap<ResourceId, crate::layout::Rect> {
    workspace
        .render_window(zoomed)
        .filter(|ls| ls.tree.is_some())
        .map(|ls| {
            crate::attach::paint::with_tiling(ls.as_ref(), content, viewport_dims, |tiling| {
                tiling.rects.clone()
            })
        })
        .unwrap_or_default()
}

/// Emit `RESIZE_TERMINAL` for each pane whose rect changed from `prev_rects`
/// (after a zoom or sidebar toggle), before repainting.
pub(super) async fn emit_view_reflow(
    conn: &mut Connection,
    workspace: &Workspace,
    zoomed: Option<&ResourceId>,
    prev_rects: &HashMap<ResourceId, crate::layout::Rect>,
    content: crate::layout::Rect,
) -> Result<(), AttachError> {
    let Some(ls) = workspace.render_window(zoomed) else {
        return Ok(());
    };
    emit_layout_reflow(conn, ls.as_ref(), prev_rects, content).await
}

/// Size every window of a just-restored workspace before its first full
/// paint (ATTACHED only carried a one-pane fallback).
pub(super) async fn emit_bootstrap_workspace_reflow(
    conn: &mut Connection,
    workspace: &Workspace,
    content: crate::layout::Rect,
) -> Result<(), AttachError> {
    let no_previous_rects = HashMap::new();
    for window in &workspace.windows {
        emit_layout_reflow(conn, &window.state, &no_previous_rects, content).await?;
    }
    Ok(())
}

/// Emit the resize diff for one concrete window layout.
async fn emit_layout_reflow(
    conn: &mut Connection,
    layout: &LayoutState,
    prev_rects: &HashMap<ResourceId, crate::layout::Rect>,
    content: crate::layout::Rect,
) -> Result<(), AttachError> {
    let diff = crate::attach::reflow::compute_reflow(layout, prev_rects, content);
    for (terminal_id, new_rect) in &diff.changed {
        // A leaf a persisted layout names before its ATTACH_RESOURCE is
        // confirmed has no QUIC stream yet, and may name a pane that died
        // with a previous server. It is sized once its stream binds.
        if !conn.can_route_terminal(terminal_id) {
            tracing::debug!(?terminal_id, "reflow: skipping a pane with no stream yet");
            continue;
        }
        conn.send(&FrameKind::ResizeTerminal {
            terminal_id: terminal_id.clone(),
            cols: new_rect.w,
            rows: new_rect.h,
        })
        .await?;
    }
    Ok(())
}

/// A `VIEWPORT_RESIZE` frame for `viewport`.
pub(super) const fn viewport_resize_frame(viewport: ViewportInfo) -> FrameKind {
    FrameKind::ViewportResize { viewport }
}

/// The current viewport, or 80x24 (logged) if the query fails.
pub(super) fn current_viewport_or_default() -> ViewportInfo {
    match current_viewport() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(error = %err, "tcgetwinsize failed; falling back to 80x24");
            ViewportInfo::new(80, 24)
        }
    }
}

/// Per-cell pixel fallback; MUST equal the server's `DEFAULT_CELL_PX` so
/// `INPUT_MOUSE` positions quantize back to the same cell (SPEC input.md
/// §3.1).
pub(super) const HOST_CELL_PX_FALLBACK: (u16, u16) = (8, 16);

/// The host's per-cell pixel size, floored exactly as the server derives it
/// (SPEC L1 §9.2.1).
pub(super) fn host_cell_px(viewport: &ViewportInfo) -> (u16, u16) {
    let derived = (|| {
        if viewport.cols == 0 || viewport.rows == 0 {
            return None;
        }
        let w = viewport.pixel_w? / viewport.cols;
        let h = viewport.pixel_h? / viewport.rows;
        (w > 0 && h > 0).then_some((w, h))
    })();
    derived.unwrap_or(HOST_CELL_PX_FALLBACK)
}

/// Read the controlling-TTY size via `tcgetwinsize` and return the
/// matching [`ViewportInfo`]. Pixel dimensions are reported when the
/// kernel provides them.
pub(super) fn current_viewport() -> Result<ViewportInfo, AttachError> {
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        // Fall back to a sane default if stdout isn't a TTY (rare for the
        // attach path; the early TTY check should have caught this).
        return Ok(ViewportInfo::new(80, 24));
    }
    let size = rustix::termios::tcgetwinsize(stdout.as_fd())
        .map_err(|err| AttachError::Terminal(format!("tcgetwinsize: {err}")))?;
    let pixel_w = if size.ws_xpixel == 0 {
        None
    } else {
        Some(size.ws_xpixel)
    };
    let pixel_h = if size.ws_ypixel == 0 {
        None
    } else {
        Some(size.ws_ypixel)
    };
    Ok(ViewportInfo::new(size.ws_col, size.ws_row).with_pixels(pixel_w, pixel_h))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use tokio::net::UnixStream;

    /// A restored workspace reflow must include off-screen windows. They are
    /// not painted yet, but their first ordinary paint must not depend on a
    /// later window switch doing a corrective resize.
    #[tokio::test]
    async fn restored_workspace_reflow_sizes_panes_in_every_window() {
        let first = ResourceId::local(1);
        let second = ResourceId::local(2);
        let mut workspace = Workspace::single(first);
        workspace.add_window("2".to_owned(), second.clone());
        workspace.select(0);

        let viewport = (100, 30);
        let content = crate::layout::Rect {
            x: 0,
            y: 0,
            w: viewport.0,
            h: viewport.1,
        };
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut client = Connection::from_stream(client_stream);
        let mut server = Connection::from_stream(server_stream);
        let (sent, received) = tokio::join!(
            emit_bootstrap_workspace_reflow(&mut client, &workspace, content),
            async {
                [
                    server.recv().await.expect("first resize frame"),
                    server.recv().await.expect("second resize frame"),
                ]
            },
        );

        drop(client);
        drop(server);
        sent.expect("bootstrap reflow sends");
        let resized: std::collections::HashSet<_> = received
            .into_iter()
            .map(|frame| match frame {
                FrameKind::ResizeTerminal {
                    terminal_id,
                    cols: 100,
                    rows: 30,
                } => terminal_id,
                other => panic!("expected 100x30 resize, got {other:?}"),
            })
            .collect();
        assert_eq!(
            resized,
            [ResourceId::local(1), second].into_iter().collect()
        );
    }
}
