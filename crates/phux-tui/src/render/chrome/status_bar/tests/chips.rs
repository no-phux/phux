//! The bar's overlays: notices, badges, the attention hint, and the
//! prefix chip.

#[allow(clippy::wildcard_imports)]
use super::*;

/// A notice strip laid by char would emit `cols + 1` columns when a wide
/// character hit the last column, wrapping the bar into the panes. A
/// straddling character is dropped whole.
#[test]
fn a_notice_strip_advances_exactly_its_own_width() {
    let style = Style::default();
    for message in [
        "\u{65e5}\u{672c}\u{8a9e}のエラー",
        "config error: \u{65e5}",
        "a\u{65e5}b\u{672c}c",
        "plain ascii notice",
    ] {
        for cols in 1u16..24 {
            let buffer = full_row_buffer(message, style, cols);
            assert_eq!(
                emitted_columns(&buffer, cols),
                cols,
                "{message:?} at {cols}"
            );
        }
    }
    let buffer = full_row_buffer("a\u{65e5}", style, 2);
    assert_eq!(
        (buffer[(0, 0)].symbol(), buffer[(1, 0)].symbol()),
        ("a", " ")
    );
    let buffer = full_row_buffer("a\u{65e5}", style, 3);
    assert_eq!(buffer[(1, 0)].symbol(), "\u{65e5}");
}

/// A notice may quote the user's config; neither a control character nor
/// a bidi override may become a cell of the raw-VT strip.
#[test]
fn a_notice_cannot_carry_controls_or_bidi_overrides() {
    let buffer = full_row_buffer("a\u{1b}[31m\u{202e}b", Style::default(), 10);
    let row: String = (0..10).map(|x| buffer[(x, 0)].symbol()).collect();
    assert!(
        !row.contains('\u{1b}') && !row.contains('\u{202e}'),
        "{row:?}"
    );
    assert!(row.starts_with("a[31mb"), "inert payload survives: {row:?}");
}

/// A notice floats over the bar as a compact right-aligned toast, the
/// newest wins, it masks covered hit targets, and it expires at the TTL.
#[test]
fn notice_toast_lifecycle() {
    let mut p = session_bar(Position::Bottom);
    let now = std::time::Instant::now();
    assert!(p.set_notice(Notice::info("first notice"), now));
    let v = visible(&mut p, 40, 24, "sess");
    assert!(v.contains("first notice") && v.contains("sess"), "{v:?}");
    assert!(p.set_notice(Notice::warn("second notice"), now));
    let v = visible(&mut p, 40, 24, "sess");
    assert!(
        v.contains("second notice") && !v.contains("first notice"),
        "{v:?}"
    );
    assert!(!p.clear_expired_notice(now + Duration::from_secs(6)));
    assert!(p.clear_expired_notice(now + NOTICE_TTL));
    assert!(!p.clear_expired_notice(now + NOTICE_TTL));
    let v = visible(&mut p, 40, 24, "sess");
    assert!(v.contains("sess") && !v.contains("second notice"), "{v:?}");

    let mut p = session_bar(Position::Top);
    assert!(p.set_notice(Notice::warn("pane 4: exited 127"), now));
    let (buffer, _, _) = p
        .compose_buffer(BarInset::NONE, 40, 24, &ctx_default("main"))
        .expect("composes");
    let row: String = (0..40).map(|x| buffer[(x, 0)].symbol()).collect();
    assert!(
        row.starts_with("main") && row.ends_with(" pane 4: exited 127 "),
        "{row:?}"
    );

    let cfg = StatusCfg {
        right: vec![Widget::Bare("windows".into())],
        ..Default::default()
    };
    let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Top);
    p.set_windows(wins(&[("shell", true)]));
    assert!(p.set_notice(Notice::warn("pane 4: exited 127"), now));
    paint(&mut p, BarInset::NONE, 40, 24, "main");
    assert_eq!(p.hit_at(39), None, "the toast masks the tab beneath it");
}

/// The error line outranks a notice, and an empty bar never reserves a
/// row for one.
#[test]
fn notices_are_refused_without_a_bar_row_of_their_own() {
    let now = std::time::Instant::now();
    let mut p = StatusBarPainter::error_line("config error: boom");
    assert!(!p.set_notice(Notice::warn("pane 3: exited 137"), now));
    let v = visible(&mut p, 40, 24, "");
    assert!(
        v.contains("config error") && !v.contains("exited 137"),
        "{v:?}"
    );

    let mut p = StatusBarPainter::new(build_bar(&StatusCfg::default()), Position::Bottom);
    assert!(!p.set_notice(Notice::warn("federation degraded"), now));
    assert!(p.is_empty());
    assert_eq!(p.min_poll_interval(), None);
    assert!(paint(&mut p, BarInset::NONE, 80, 24, "").is_empty());
}

#[test]
fn badges_mask_underlying_click_targets_in_an_inset_bar() {
    use phux_config::widget::CellHit;
    for hit in [CellHit::Switch, CellHit::Window(2)] {
        let mut p = windows_bar();
        p.supervisory = Some("[ FROZEN ]".to_owned());
        p.attention = Some("[ ASK ]".to_owned());
        let row = vec![
            WidgetCell {
                hit: Some(hit),
                ..WidgetCell::default()
            };
            24
        ];
        p.last_row = Some((36, 24, row));
        for col in 0..24 {
            // 7 attention cells, a blank separator, 10 badge cells.
            assert_eq!(p.hit_at(36 + col), if col < 6 { Some(hit) } else { None });
        }
        assert_eq!(p.badge_gap(24), Some(13));
        assert_eq!(p.hit_at(35), None);
        assert_eq!(p.hit_at(60), None);
    }
}

/// The attention chip right-aligns in the theme color, shifts left of a
/// supervisory badge, and stops painting once cleared.
#[test]
fn attention_hint_placement_and_clearing() {
    let mut p = windows_bar();
    p.set_windows(wins(&[("a", true)]));
    assert!(visible(&mut p, 40, 10, "").contains("0:a"));
    p.set_theme(&Theme {
        attention: Color::Rgb(251, 191, 36),
        ..Theme::default()
    });
    assert!(p.set_attention(Some("[ ASK ]".to_owned())));
    assert!(!p.set_attention(Some("[ ASK ]".to_owned())));
    let s = paint(&mut p, BarInset::NONE, 40, 10, "");
    assert!(
        s.contains("\x1b[10;34H") && s.contains("\x1b[38;2;251;191;36m"),
        "{s:?}"
    );
    assert!(strip_csi(&s).contains("[ ASK ]"));

    assert!(p.set_supervisory(Some("[ FROZEN ]".to_owned())));
    let s = paint(&mut p, BarInset::NONE, 40, 10, "");
    // Badge at cols 31..40; the chip right-aligns 11 cells further left.
    assert!(
        s.contains("\x1b[10;31H") && s.contains("\x1b[10;23H"),
        "{s:?}"
    );

    assert!(p.set_attention(None));
    assert!(!visible(&mut p, 40, 10, "").contains("ASK"));
}

/// The prefix chip sits at the right edge in the accent colour behind a
/// one-cell gap, swallows clicks, and goes with the pending state.
#[test]
fn prefix_chip_rides_the_right_edge_until_cleared() {
    let mut p = windows_bar();
    p.set_windows(wins(&[("a", true)]));
    p.set_theme(&Theme {
        accent: Color::Rgb(190, 230, 80),
        ..Theme::default()
    });
    assert!(p.set_prefix_pending(true));
    assert!(!p.set_prefix_pending(true), "already pending");
    let s = paint(&mut p, BarInset::NONE, 40, 10, "");
    // " C-a " is five cells: columns 35..40, so CUP to the 36th.
    assert!(
        s.contains("\x1b[10;36H") && s.contains("\x1b[38;2;190;230;80m"),
        "{s:?}"
    );
    assert!(strip_csi(&s).contains(" C-a "));
    assert_eq!(p.key_gap(40), Some(34));
    for col in 34..40 {
        assert_eq!(p.hit_at(col), None, "the chip and its gap are not a target");
    }

    assert!(p.set_prefix_pending(false));
    assert!(!visible(&mut p, 40, 10, "").contains("C-a"));
    assert!(!p.set_prefix_pending(false), "already clear");
}

/// A right-aligned widget moves left of the pending prefix chip whole;
/// the chip never cuts into its text.
#[test]
fn the_prefix_chip_moves_right_widgets_instead_of_cutting_them() {
    let cfg = StatusCfg {
        left: vec![spec("windows", &[])],
        right: vec![spec("session-name", &[])],
        ..StatusCfg::default()
    };
    let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
    p.set_windows(wins(&[("a", true)]));
    let before = visible(&mut p, 40, 10, "work");
    assert!(before.trim_end().ends_with("work"), "{before:?}");

    assert!(p.set_prefix_pending(true));
    let during = visible(&mut p, 40, 10, "work");
    let work = during
        .find("work")
        .expect("the session name survives whole");
    let chip = during.find(" C-a ").expect("the chip");
    assert!(
        work + 4 < chip,
        "the name sits left of the chip: {during:?}"
    );

    assert!(p.set_prefix_pending(false));
    let after = visible(&mut p, 40, 10, "work");
    assert!(after.trim_end().ends_with("work"), "{after:?}");
}

/// The active key mode rides the same chip; the pending prefix outranks
/// it, and clearing either leaves the other showing.
#[test]
fn the_key_mode_rides_the_chip_until_the_prefix_or_its_exit() {
    let mut p = windows_bar();
    p.set_windows(wins(&[("a", true)]));
    assert!(p.set_key_mode(Some("resize")));
    assert!(!p.set_key_mode(Some("resize")), "already showing");
    assert!(visible(&mut p, 40, 10, "").ends_with(" resize "));

    assert!(p.set_prefix_pending(true));
    let during = visible(&mut p, 40, 10, "");
    assert!(
        during.ends_with(" C-a ") && !during.contains("resize"),
        "{during:?}"
    );
    assert!(p.set_prefix_pending(false));
    assert!(visible(&mut p, 40, 10, "").ends_with(" resize "));

    assert!(p.set_key_mode(None));
    assert!(!visible(&mut p, 40, 10, "").contains("resize"));
    let mut error = StatusBarPainter::error_line("broken");
    assert!(
        !error.set_key_mode(Some("resize")),
        "never over the error line"
    );
}
