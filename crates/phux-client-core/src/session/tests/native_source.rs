//! Native source fixture with enough history to require multiple page units.

use crate::engine::ghostty::snapshot::{EncodedSnapshot, SnapshotCaptureLimits};
use libghostty_vt::Terminal;

pub(super) fn capture(limits: SnapshotCaptureLimits) -> EncodedSnapshot {
    let mut terminal = Terminal::new(200, 3).expect("source terminal");
    terminal
        .set_scrollback_max_lines(Some(5_000))
        .expect("history rows");
    terminal
        .set_scrollback_max_bytes(Some(usize::MAX))
        .expect("history bytes");
    for row in 0..3_000 {
        terminal.vt_write(format!("wire-history-{row:04}\r\n").as_bytes());
    }
    EncodedSnapshot::capture(&mut terminal, limits).expect("native capture")
}
