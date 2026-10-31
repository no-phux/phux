use super::*;
use base64::Engine as _;

/// `Some(n)` includes exactly `n` history rows.
#[test]
fn render_screen_some_n_boundary_includes_exactly_n_history_rows() {
    // 5 lines, 20x2 viewport -> 3 rows of scrollback (line1..line3),
    // matching `screen_state_with_scrollback_bounds_to_recent_rows`.
    let mut t = fresh(20, 2);
    t.vt_write(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");
    assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 3);

    let synth = SnapshotSynthesizer::new().expect("synth");
    let rendered = synth
            .render_screen(&t, Some(2), 2) // format 2 = vt
            .expect("render_screen")
            .expect("format 2 must render");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&rendered.data)
        .expect("valid base64");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("line2") && text.contains("line3"),
        "the most-recent 2 rows (n=2) must be included, got: {text}",
    );
    assert!(
        !text.contains("line1"),
        "the 3rd-from-end row (n+1) must be excluded, got: {text}",
    );
}

/// The rendered history window never exceeds `ROW_WINDOW_MAX` rows.
#[test]
fn render_screen_clamps_all_retained_history_to_row_window_max() {
    let mut t = GhosttyTerminal::new(10, 2).expect("Terminal::new");
    let over = usize::try_from(ROW_WINDOW_MAX).unwrap_or(usize::MAX) + 5;
    t.set_scrollback_max_lines(Some(over + 10))
        .expect("set_scrollback_max_lines");
    // Lift the byte cap so the clamp under test, not retention, bounds it.
    t.set_scrollback_max_bytes(None)
        .expect("set_scrollback_max_bytes");
    let mut input = Vec::with_capacity(over * 4);
    for i in 0..over {
        input.extend_from_slice(format!("r{i}\r\n").as_bytes());
    }
    input.extend_from_slice(b"last");
    t.vt_write(&input);
    assert!(
        t.scrollback_rows().expect("scrollback_rows")
            > usize::try_from(ROW_WINDOW_MAX).unwrap_or(0),
        "the terminal must actually retain more than ROW_WINDOW_MAX for this test to mean anything",
    );

    let synth = SnapshotSynthesizer::new().expect("synth");
    // format 1 = html: cheaper to search as plain text than base64-vt.
    let rendered = synth
        .render_screen(&t, Some(SCROLLBACK_ALL), 1)
        .expect("render_screen")
        .expect("format 1 must render");
    assert!(
        rendered.data.contains(&format!("r{}", over - 1)),
        "the most-recent retained row must still be present",
    );
    assert!(
        !rendered.data.contains("r0"),
        "the oldest retained row must have been clamped away by ROW_WINDOW_MAX \
             (data omitted from this message; it is large by construction)",
    );
}

/// Over budget is refused, not truncated.
#[test]
fn render_screen_over_budget_is_refused_not_truncated() {
    let mut t = fresh(20, 2);
    t.vt_write(b"hi");

    let err = SnapshotSynthesizer::render_screen_with_budget(&t, None, 1, 1)
        .expect_err("a one-byte budget must refuse any non-trivial render");
    match err {
        SynthesisError::RenderBudgetExceeded { required, budget } => {
            assert_eq!(budget, 1);
            assert!(required > budget, "got required={required}");
        }
        other => panic!("expected RenderBudgetExceeded, got {other:?}"),
    }
}

/// Within budget renders normally.
#[test]
fn render_screen_under_budget_renders_normally() {
    let mut t = fresh(20, 2);
    t.vt_write(b"hi");

    let rendered = SnapshotSynthesizer::render_screen_with_budget(&t, None, 1, RENDER_BUDGET_BYTES)
        .expect("render_screen_with_budget")
        .expect("format 1 must render");
    assert_eq!(rendered.format, RENDERED_FORMAT_HTML);
    assert!(rendered.data.contains("hi"));
}
