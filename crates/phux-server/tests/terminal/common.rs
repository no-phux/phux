//! Helpers shared by the terminal suites.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::time::Duration;

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, StateScope};
use phux_server::terminal_actor::PaneOutput;
use portable_pty::CommandBuilder;
use tokio::net::UnixStream;
use tokio::sync::broadcast;

use phux_server_testkit::command;

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

/// Count non-overlapping occurrences of `needle` in `hay`.
pub(crate) fn count(hay: &[u8], needle: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
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

/// `GET_STATE { Server }` -> the focused pane of the seeded session.
pub(crate) async fn focused_resource(stream: &mut UnixStream, request_id: u32) -> ResourceId {
    let scope = StateScope::Server;
    match command(stream, request_id, Command::GetState { scope }).await {
        CommandResult::OkWith(CommandValue::State(snap)) => snap.focused_resource,
        other => panic!("expected Ok_With(State(..)), got {other:?}"),
    }
}

/// `GET_SCREEN` for `pane`, parsed.
pub(crate) async fn screen(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &ResourceId,
) -> phux_core::screen::ScreenState {
    let get = Command::GetScreen {
        terminal_id: pane.clone(),
        request_scrollback: None,
        cells: false,
        format: 0,
    };
    match command(stream, request_id, get).await {
        CommandResult::OkWith(CommandValue::Json(json)) => serde_json::from_str(&json).unwrap(),
        other => panic!("expected Ok_With(Json(..)), got {other:?}"),
    }
}

/// Poll `GET_SCREEN` (up to ~5s) until `done` holds; returns the last screen
/// either way so the caller's assertion carries diagnostics.
pub(crate) async fn poll_screen(
    stream: &mut UnixStream,
    pane: &ResourceId,
    done: impl Fn(&phux_core::screen::ScreenState) -> bool,
) -> phux_core::screen::ScreenState {
    let mut last = None;
    for i in 0..200 {
        let s = screen(stream, 2000 + i, pane).await;
        if done(&s) {
            return s;
        }
        last = Some(s);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    last.unwrap()
}
