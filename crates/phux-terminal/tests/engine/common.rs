//! Helpers shared by the engine suites.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::time::Duration;

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_terminal::actor::PaneOutput;
use phux_terminal::pty::CommandBuilder;
use tokio::sync::broadcast;

/// `/bin/sh -c script`.
pub(crate) fn sh(script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", script]);
    cmd
}

/// An unmodified press of a text-less key (Enter, Escape, arrows, ...).
pub(crate) const fn named_key(key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// A 200-line-scrollback terminal.
pub(crate) fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    let mut terminal = GhosttyTerminal::new(cols, rows).unwrap();
    terminal.set_scrollback_max_lines(Some(200)).unwrap();
    terminal
}

/// Viewport rows as right-trimmed text, skipping wide-cell tails.
pub(crate) fn render_grid(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    let rows_n = usize::from(snap.rows().unwrap());
    let mut row_storage = RowIterator::new().unwrap();
    let mut cell_storage = CellIterator::new().unwrap();
    let mut row_iter = row_storage.update(&snap).unwrap();
    let mut grid = Vec::with_capacity(rows_n);
    while grid.len() < rows_n
        && let Some(row) = row_iter.next()
    {
        let mut line = String::new();
        let mut cells = cell_storage.update(row).unwrap();
        while let Some(cell) = cells.next() {
            if matches!(
                cell.raw_cell().unwrap().wide().unwrap(),
                CellWide::SpacerTail
            ) {
                continue;
            }
            let g = cell.graphemes().unwrap();
            if g.is_empty() {
                line.push(' ');
            } else {
                line.extend(g);
            }
        }
        grid.push(line.trim_end().to_owned());
    }
    grid
}

/// Drain a pane's output broadcast until `needle` appears. A lag is a hard
/// failure: callers compare byte streams, so dropped chunks void the check.
pub(crate) async fn collect_until(
    rx: &mut broadcast::Receiver<PaneOutput>,
    needle: &[u8],
) -> Vec<u8> {
    let work = async {
        let mut acc = Vec::new();
        loop {
            match rx.recv().await {
                Ok(PaneOutput::Live { bytes, .. } | PaneOutput::Resync { bytes, .. }) => {
                    acc.extend_from_slice(&bytes);
                    if acc.windows(needle.len()).any(|w| w == needle) {
                        return acc;
                    }
                }
                Ok(PaneOutput::Control { .. }) => {}
                Err(e) => panic!("broadcast {e:?} before {needle:?}; acc={acc:?}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), work)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle:?} on the broadcast"))
}
