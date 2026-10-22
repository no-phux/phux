//! The closed opts surface and the universal widget-level `style`.

use super::*;

// ---------------------------------------------------------------------------
// Closed opts surface (phux-i0e8.4.2): every factory rejects unknown
// options, naming the widget and suggesting the nearest valid opt.
// ---------------------------------------------------------------------------

/// One rejection case: widget kind, opts, expected suggestion.
type RejectionCase<'a> = (&'a str, Vec<(&'a str, toml::Value)>, &'a str);

#[test]
fn every_factory_rejects_unknown_opts_with_a_suggestion() {
    // One near-miss typo per kind; each must be rejected by *its* factory
    // (kind named in the error) with a did-you-mean for the real opt.
    let cases: &[RejectionCase<'_>] = &[
        (
            "time",
            vec![("formt", toml::Value::String("%H".to_owned()))],
            "format",
        ),
        (
            "session-name",
            vec![("prefx", toml::Value::String("s:".to_owned()))],
            "prefix",
        ),
        (
            "cwd",
            vec![("truncat", toml::Value::Integer(8))],
            "truncate",
        ),
        (
            "exit",
            vec![("forma", toml::Value::String("{code}".to_owned()))],
            "format",
        ),
        (
            "windows",
            vec![("separater", toml::Value::String("|".to_owned()))],
            "separator",
        ),
        (
            "exec",
            vec![
                ("command", toml::Value::String("true".to_owned())),
                ("intervall", toml::Value::String("5s".to_owned())),
            ],
            "interval",
        ),
    ];
    for (kind, opts, want_suggestion) in cases {
        match build_spec(kind, opts) {
            Err(WidgetError::InvalidOption { kind: k, message }) => {
                assert_eq!(&k, kind, "error names the wrong widget: {message}");
                assert!(
                    message.contains("unknown option"),
                    "{kind}: wrong message: {message}"
                );
                assert!(
                    message.contains(&format!("did you mean `{want_suggestion}`?")),
                    "{kind}: no suggestion in: {message}"
                );
            }
            other => panic!("{kind}: expected InvalidOption, got {other:?}"),
        }
    }
}

#[test]
fn a_typoed_style_key_is_rejected_and_suggests_style() {
    // `style` is consumed by the registry, but it is still a valid
    // spelling — a near-miss must point at it.
    let err = build_spec(
        "time",
        &[(
            "styel",
            style_table(&[("bold", toml::Value::Boolean(true))]),
        )],
    )
    .expect_err("typo'd style rejected");
    match err {
        WidgetError::InvalidOption { message, .. } => {
            assert!(message.contains("did you mean `style`?"), "{message}");
        }
        other @ WidgetError::UnknownKind(_) => panic!("expected InvalidOption, got {other:?}"),
    }
}

// --- Universal widget-level `style` (phux-i0e8.4.2)

fn red_bold() -> toml::Value {
    style_table(&[
        ("fg", toml::Value::String("red".to_owned())),
        ("bold", toml::Value::Boolean(true)),
    ])
}

#[test]
fn style_opt_styles_time_session_name_cwd_and_exit_cells() {
    let want = CellStyle {
        fg: Some("red".to_owned()),
        bold: true,
        ..CellStyle::default()
    };
    // (kind, extra opts) — each renders at least one cell in the fixture
    // context below and every cell must carry the widget-level style.
    let cases: &[(&str, Vec<(&str, toml::Value)>)] = &[
        // Literal format keeps the time widget deterministic.
        (
            "time",
            vec![("format", toml::Value::String("T".to_owned()))],
        ),
        ("session-name", vec![]),
        ("cwd", vec![]),
        ("exit", vec![]),
    ];
    for (kind, extra) in cases {
        let mut opts = extra.clone();
        opts.push(("style", red_bold()));
        let w = build_spec(kind, &opts).unwrap_or_else(|e| panic!("{kind} builds: {e}"));
        let ctx = WidgetContext {
            cwd: "/tmp",
            last_exit: Some(0),
            ..WidgetContext::new(fixed_time(), "main", "C-a", &[])
        };
        let cells = w.render(&ctx);
        assert!(!cells.is_empty(), "{kind} rendered nothing");
        for cell in &cells.cells {
            assert_eq!(
                cell.style.as_ref(),
                Some(&want),
                "{kind}: cell {:?} not styled",
                cell.text
            );
        }
    }
}

#[test]
fn per_cell_styles_win_over_the_widget_style() {
    // The windows widget styles its segments itself (active bold+reverse,
    // inactive dim); only the unstyled separator inherits the widget-level
    // style. That is the documented precedence (tui.md §8.3).
    let cells = render_windows(&[("style", red_bold())], &[win("a", true), win("b", false)]);
    assert_eq!(text_of(&cells), "0:a 1:b");
    let active = cells.cells[0].style.clone().expect("active styled");
    assert!(active.bold && active.reverse, "active keeps its own style");
    assert_eq!(active.fg, None, "widget style must not leak into active");
    let separator = &cells.cells[3];
    let sep_style = separator.style.clone().expect("separator inherits");
    assert_eq!(sep_style.fg.as_deref(), Some("red"));
    assert!(sep_style.bold && !sep_style.reverse);
}

#[test]
fn a_plain_style_table_is_a_no_op() {
    let w = build_spec("session-name", &[("style", style_table(&[]))]).unwrap();
    let cells = w.render(&WidgetContext::new(fixed_time(), "main", "C-a", &[]));
    assert!(cells.cells.iter().all(|c| c.style.is_none()));
}

#[test]
fn a_bad_style_table_is_rejected_naming_the_widget() {
    for bad in [
        toml::Value::String("red".to_owned()),
        style_table(&[("colour", toml::Value::String("red".to_owned()))]),
    ] {
        match build_spec("time", &[("style", bad.clone())]) {
            Err(WidgetError::InvalidOption { kind, message }) => {
                assert_eq!(kind, "time");
                assert!(
                    message.contains("`style` must be a style table"),
                    "{message}"
                );
            }
            other => panic!("style {bad:?}: expected InvalidOption, got {other:?}"),
        }
    }
}

#[test]
fn styled_wrapper_forwards_poll_interval_and_exec_feed() {
    let time = build_spec("time", &[("style", red_bold())]).unwrap();
    assert_eq!(time.poll_interval(), Some(Duration::from_secs(1)));
    let exec = build_spec(
        "exec",
        &[
            ("command", toml::Value::String("true".to_owned())),
            ("style", red_bold()),
        ],
    )
    .unwrap();
    assert!(exec.exec_feed().is_some(), "exec feed lost behind Styled");
}

/// A window running an agent shows the agent's glyph before its name, in
/// the badge's own ink on the tab's bed, and the whole tab stays one click
/// target.
#[test]
fn windows_widget_paints_the_badge_before_the_name() {
    let active = style_table(&[("bg", toml::Value::String("#293628".to_owned()))]);
    let window = WindowInfo {
        badge: Some(WindowBadge {
            glyph: "\u{25d0}".to_owned(),
            style: CellStyle {
                fg: Some("#86efac".to_owned()),
                ..CellStyle::default()
            },
        }),
        ..win("claude", true)
    };
    let cells = render_windows(
        &[
            ("format", toml::Value::String(" {index} {name} ".to_owned())),
            ("active", active),
        ],
        &[window],
    );
    assert_eq!(text_of(&cells), " 0 \u{25d0} claude ");
    let glyph = &cells.cells[3];
    let style = glyph.style.as_ref().expect("badge styled");
    assert_eq!(style.fg.as_deref(), Some("#86efac"));
    assert_eq!(
        style.bg.as_deref(),
        Some("#293628"),
        "badge sits on the tab's bed"
    );
    assert!(
        cells
            .cells
            .iter()
            .all(|c| c.hit == Some(CellHit::Window(0)))
    );
}

/// `index` inks only the selector; the name keeps the segment style.
#[test]
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "`{index}`/`{name}` are the widget's own template placeholders"
)]
fn windows_widget_index_style_touches_only_the_index() {
    let index = style_table(&[("fg", toml::Value::String("#7c8696".to_owned()))]);
    let inactive = style_table(&[("fg", toml::Value::String("#9aa4b2".to_owned()))]);
    let cells = render_windows(
        &[
            ("format", toml::Value::String("{index} {name}".to_owned())),
            ("inactive", inactive),
            ("index", index),
        ],
        &[win("zsh", false), win("nvim", true)],
    );
    let fg = |i: usize| cells.cells[i].style.as_ref().and_then(|s| s.fg.clone());
    assert_eq!(fg(0).as_deref(), Some("#7c8696"), "index");
    assert_eq!(fg(2).as_deref(), Some("#9aa4b2"), "name");
}
