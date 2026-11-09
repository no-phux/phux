//! Deep native source used by the bounded history-page benchmark.

use libghostty_vt::Terminal;
use phux_client_core::engine::ghostty::snapshot::{EncodedSnapshot, SnapshotCaptureLimits};

/// Capture multiple pages of deterministic history with the stock encoder.
///
/// # Panics
/// Panics if terminal allocation, configuration, or bounded capture fails.
#[must_use]
pub fn capture(limits: SnapshotCaptureLimits) -> EncodedSnapshot {
    let mut terminal = Terminal::new(200, 3).expect("native benchmark terminal");
    terminal
        .set_scrollback_max_lines(Some(5_000))
        .expect("scrollback rows");
    terminal
        .set_scrollback_max_bytes(Some(usize::MAX))
        .expect("scrollback bytes");
    for row in 0..3_000 {
        terminal.vt_write(format!("native-history-{row:04}\r\n").as_bytes());
    }
    EncodedSnapshot::capture(&mut terminal, limits).expect("native capture")
}
