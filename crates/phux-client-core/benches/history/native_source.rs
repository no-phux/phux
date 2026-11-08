//! Deep native source used by the bounded history-page benchmark.

use libghostty_vt::Terminal;

/// Build a source with multiple pages of deterministic history.
///
/// # Panics
/// Panics if terminal allocation or scrollback configuration fails.
#[must_use]
pub fn terminal() -> Terminal<'static, 'static> {
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
    terminal
}
