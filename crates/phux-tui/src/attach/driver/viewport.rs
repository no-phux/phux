//! Outer-terminal viewport reads, the SIGWINCH frame builder, and the
//! per-leaf PTY reflow emitters.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::os::fd::AsFd;

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, ViewportInfo};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::layout::{LayoutState, Workspace};

/// The per-leaf rect map of the zoom- and sidebar-honoring view, used as the
/// pre-toggle snapshot for the reflow handshake. Returns an empty map when
/// there is no active window or its tree is unseeded (single-pane bootstrap).
///
/// phux-l96p.4: this runs on every input batch — the reflow handshake needs a
/// *pre*-dispatch snapshot, so it cannot be deferred behind the "did zoom or
/// the sidebar move?" test it feeds. It therefore goes through the paint
/// path's memoized tiling rather than calling `compute_layout_in` directly:
/// the tiling inputs (layout, content rect, viewport) are unchanged between
/// keystrokes, so the steady state is a cache hit and the per-keystroke cost
/// drops to cloning the rect map. Same tiling, same rects — the cache key is
/// the complete input, so a hit is exact.
pub(super) fn view_rects(
    workspace: &Workspace,
    zoomed: Option<&TerminalId>,
    content: crate::layout::Rect,
    viewport_dims: (u16, u16),
) -> HashMap<TerminalId, crate::layout::Rect> {
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

/// Emit one `TERMINAL_RESIZE` per pane whose dimensions differ between
/// `prev_rects` and the new content view. Reuses the close/SIGWINCH reflow
/// path so each PTY's winsize tracks the on-screen geometry. Sent before
/// repainting, mirroring the other reflow sites.
///
/// Called on a pane-zoom or sidebar toggle with the pre-toggle rects.
pub(super) async fn emit_view_reflow(
    conn: &mut Connection,
    workspace: &Workspace,
    zoomed: Option<&TerminalId>,
    prev_rects: &HashMap<TerminalId, crate::layout::Rect>,
    content: crate::layout::Rect,
) -> Result<(), AttachError> {
    let Some(ls) = workspace.render_window(zoomed) else {
        return Ok(());
    };
    emit_layout_reflow(conn, ls.as_ref(), prev_rects, content).await
}

/// Seed every window in a restored workspace before its first full paint.
///
/// The initial `ATTACHED` frame contains only a single-pane fallback; the
/// persisted workspace arrives later through the layout metadata reply. At
/// that point every PTY already exists and still has the outer-terminal size.
/// Reflowing it before the reply's full paint lets a later window switch be an
/// ordinary paint rather than a corrective resize.
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
    prev_rects: &HashMap<TerminalId, crate::layout::Rect>,
    content: crate::layout::Rect,
) -> Result<(), AttachError> {
    let diff = crate::attach::reflow::compute_reflow(layout, prev_rects, content);
    for (terminal_id, new_rect) in &diff.changed {
        conn.send(&FrameKind::TerminalResize {
            terminal_id: terminal_id.clone(),
            cols: new_rect.w,
            rows: new_rect.h,
        })
        .await?;
    }
    Ok(())
}

/// Build a `VIEWPORT_RESIZE` frame from a [`ViewportInfo`].
///
/// Pure function, factored out of [`main_loop`] so unit tests can
/// exercise the encoder-feeding side without firing a real SIGWINCH or
/// driving a tokio runtime. The wire shape matches SPEC §7.1 / §10.5.
pub(super) const fn viewport_resize_frame(viewport: ViewportInfo) -> FrameKind {
    FrameKind::ViewportResize { viewport }
}

/// Read the current viewport, falling back to 80x24 with a logged
/// warning if the kernel query fails. Used by the SIGWINCH branch
/// where we'd rather ship a stale-but-plausible viewport than skip
/// the upstream notification entirely.
pub(super) fn current_viewport_or_default() -> ViewportInfo {
    match current_viewport() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(error = %err, "tcgetwinsize failed; falling back to 80x24");
            ViewportInfo::new(80, 24)
        }
    }
}

/// Host per-cell pixel fallback when the outer terminal reports no pixel
/// geometry. MUST stay equal to the server's `DEFAULT_CELL_PX` (and the
/// kitty-graphics `FALLBACK_CELL_PX` in `pane_state.rs`): with no pixel report
/// the server keeps its seed cell size, and `INPUT_MOUSE` positions only
/// quantize back to the right cell if both ends assume the same geometry
/// (phux-yyex, SPEC input.md §3.1).
pub(super) const HOST_CELL_PX_FALLBACK: (u16, u16) = (8, 16);

/// Derive the host's per-cell pixel size from a [`ViewportInfo`], mirroring
/// the server's SPEC L1 §9.2.1 derivation exactly (`pixel / cells`,
/// floored; degenerate axes rejected). The dispatcher scales pane-local
/// cell coordinates by this at the `INPUT_MOUSE` send boundary, so client
/// and server must floor the same division on the same numbers.
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
        let first = TerminalId::local(1);
        let second = TerminalId::local(2);
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

        sent.expect("bootstrap reflow sends");
        let resized: std::collections::HashSet<_> = received
            .into_iter()
            .map(|frame| match frame {
                FrameKind::TerminalResize {
                    terminal_id,
                    cols: 100,
                    rows: 30,
                } => terminal_id,
                other => panic!("expected 100x30 resize, got {other:?}"),
            })
            .collect();
        assert_eq!(
            resized,
            [TerminalId::local(1), second].into_iter().collect()
        );
    }

    /// The factored builder produces a `ViewportResize` frame carrying
    /// the supplied viewport unchanged. Lets us assert the encoder-
    /// feeding side of the SIGWINCH path without firing a real signal
    /// or driving a tokio runtime.
    #[test]
    fn viewport_resize_frame_carries_viewport_unchanged() {
        let vp = ViewportInfo::new(132, 50).with_pixels(Some(1320), Some(750));
        match viewport_resize_frame(vp) {
            FrameKind::ViewportResize { viewport } => {
                assert_eq!(viewport.cols, 132);
                assert_eq!(viewport.rows, 50);
                assert_eq!(viewport.pixel_w, Some(1320));
                assert_eq!(viewport.pixel_h, Some(750));
            }
            other => panic!("expected ViewportResize, got {other:?}"),
        }
    }

    /// `current_viewport_or_default` returns _something_ even when stdout
    /// isn't a TTY (cargo test path). The exact dims aren't load-bearing
    /// — what matters is that we never return an error and always have a
    /// frame to send.
    #[test]
    fn current_viewport_or_default_never_panics() {
        let vp = current_viewport_or_default();
        // Cell dims fit in u16 by construction; just exercise the path.
        let _ = (vp.cols, vp.rows);
    }
}
