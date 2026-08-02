//! Integration tests for `phux_config::widget`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use phux_config::WidgetSpec;
use phux_config::widget::{
    CellHit, CellStyle, SessionNameWidget, StatusWidget, TimeWidget, WidgetCells, WidgetContext,
    WidgetError, WidgetRegistry, WindowInfo,
};

fn opts_with(entries: &[(&str, toml::Value)]) -> BTreeMap<String, toml::Value> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn fixed_time() -> SystemTime {
    // Avoid local-timezone variability in time-widget snapshot tests by
    // using the `session-name` widget for snapshots and only asserting
    // shape (not contents) on the time widget.
    UNIX_EPOCH + Duration::from_secs(12345)
}

fn build_spec(
    kind: &str,
    opts: &[(&str, toml::Value)],
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let spec = WidgetSpec {
        kind: kind.to_owned(),
        opts: opts_with(opts),
    };
    WidgetRegistry::with_builtins().build(&spec)
}

fn text_of(cells: &WidgetCells) -> String {
    cells.cells.iter().filter_map(|c| c.text.first()).collect()
}

fn style_table(entries: &[(&str, toml::Value)]) -> toml::Value {
    let mut t = toml::value::Table::new();
    for (k, v) in entries {
        t.insert((*k).to_owned(), v.clone());
    }
    toml::Value::Table(t)
}

/// Window fixture; flip `zoomed` / `attention` via struct update at the
/// call site.
fn win(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        name: name.to_owned(),
        active,
        zoomed: false,
        attention: false,
        branch: None,
    }
}

fn render_windows(opts: &[(&str, toml::Value)], windows: &[WindowInfo]) -> WidgetCells {
    let w = build_spec("windows", opts).expect("windows builds");
    w.render(&WidgetContext::new(fixed_time(), "", "C-a", windows))
}

// ---------------------------------------------------------------------------
// Registry construction
// ---------------------------------------------------------------------------

#[test]
fn with_builtins_registers_the_shipped_widget_kinds() {
    let r = WidgetRegistry::with_builtins();
    let kinds = r.kinds();
    for kind in ["time", "session-name", "windows", "help-hints"] {
        assert!(kinds.contains(&kind), "missing {kind}: {kinds:?}");
    }
}

#[test]
fn new_starts_empty() {
    let r = WidgetRegistry::new();
    assert!(r.kinds().is_empty());
}

#[test]
fn register_then_build_invokes_factory() {
    #[allow(clippy::unnecessary_wraps)] // factory signature is fixed
    fn dummy_factory(
        _opts: &BTreeMap<String, toml::Value>,
    ) -> Result<Box<dyn StatusWidget>, WidgetError> {
        Ok(Box::new(SessionNameWidget::new(
            Some("X:".to_owned()),
            None,
        )))
    }
    let mut r = WidgetRegistry::new();
    r.register("custom", dummy_factory);
    let spec = WidgetSpec {
        kind: "custom".to_owned(),
        opts: BTreeMap::new(),
    };
    let w = r.build(&spec).expect("custom builds");
    let cells = w.render(&WidgetContext::new(fixed_time(), "main", "C-a", &[]));
    assert_eq!(text_of(&cells), "X:main");
}

// ---------------------------------------------------------------------------
// session-name widget
// ---------------------------------------------------------------------------

/// One session-name render case: description, opts, session, expected.
type SessionNameCase<'a> = (&'a str, Vec<(&'a str, toml::Value)>, &'a str, &'a str);

/// Table-driven rendering: opts x session name -> rendered text. Covers
/// prefix + truncation, the `snake_case` `max_len` alias, the no-options
/// full name (also the historical default-format behavior), `format`
/// placeholder substitution, and `format` composing with prefix/max-len
/// (phux-i0e8.4.2; tui.md section 8.3).
#[test]
fn session_name_renders_per_its_options() {
    let cases: &[SessionNameCase<'_>] = &[
        (
            "prefix and truncated name",
            vec![
                ("prefix", toml::Value::String("[sess]".to_owned())),
                ("max-len", toml::Value::Integer(4)),
            ],
            "very-long-session-name",
            "[sess]very",
        ),
        (
            "max_len snake_case alias",
            vec![("max_len", toml::Value::Integer(3))],
            "abcdef",
            "abc",
        ),
        ("no options renders full name", vec![], "main", "main"),
        (
            "format substitutes {name}",
            vec![("format", toml::Value::String("[{name}]".to_owned()))],
            "main",
            "[main]",
        ),
        (
            "format composes with prefix and max-len",
            vec![
                ("format", toml::Value::String("<{name}>".to_owned())),
                ("prefix", toml::Value::String("s:".to_owned())),
                ("max-len", toml::Value::Integer(4)),
            ],
            "very-long",
            "s:<very>",
        ),
    ];
    for (what, opts, session, want) in cases {
        let w = build_spec("session-name", opts).unwrap_or_else(|e| panic!("{what}: {e}"));
        let cells = w.render(&WidgetContext::new(fixed_time(), session, "C-a", &[]));
        assert_eq!(&text_of(&cells), want, "{what}");
    }
}

// ---------------------------------------------------------------------------
// time widget
// ---------------------------------------------------------------------------

#[test]
fn time_widget_formats_render_expected_widths() {
    // Default %H:%M renders to 5 chars (HH:MM) in any locale.
    let w = build_spec("time", &[]).expect("time builds");
    let cells = w.render(&WidgetContext::new(fixed_time(), "", "C-a", &[]));
    assert_eq!(
        cells.cells.len(),
        5,
        "expected 5 chars (HH:MM), got {}: {:?}",
        cells.cells.len(),
        text_of(&cells)
    );

    // An explicit format is honored: %Y is a 4-digit year.
    let w = build_spec("time", &[("format", toml::Value::String("%Y".to_owned()))])
        .expect("time builds");
    let cells = w.render(&WidgetContext::new(fixed_time(), "", "C-a", &[]));
    assert_eq!(cells.cells.len(), 4);
}

#[test]
fn time_widget_poll_interval_is_one_second() {
    let w = TimeWidget::new("%H:%M").expect("valid format");
    assert_eq!(w.poll_interval(), Some(Duration::from_secs(1)));
}

// ---------------------------------------------------------------------------
// Invalid options (per-kind); unknown kind
// ---------------------------------------------------------------------------

/// One invalid-option case: description, widget kind, opts.
type InvalidOptionCase<'a> = (&'a str, &'a str, Vec<(&'a str, toml::Value)>);

/// Table-driven rejection: every invalid option value must fail at build
/// time with `InvalidOption` naming the offending widget kind.
#[test]
fn invalid_option_values_are_rejected_naming_the_kind() {
    let cases: &[InvalidOptionCase<'_>] = &[
        (
            "zero max-len",
            "session-name",
            vec![("max-len", toml::Value::Integer(0))],
        ),
        (
            "non-integer max-len",
            "session-name",
            vec![("max-len", toml::Value::String("ten".to_owned()))],
        ),
        (
            "non-string session-name format",
            "session-name",
            vec![("format", toml::Value::Integer(1))],
        ),
        (
            // %Q is not a valid strftime directive.
            "invalid strftime format",
            "time",
            vec![("format", toml::Value::String("%Q".to_owned()))],
        ),
        (
            "non-string time format",
            "time",
            vec![("format", toml::Value::Integer(42))],
        ),
        (
            "non-table windows style",
            "windows",
            vec![("active", toml::Value::String("nope".to_owned()))],
        ),
        (
            "unknown windows style field",
            "windows",
            vec![(
                "inactive",
                style_table(&[("colour", toml::Value::String("red".to_owned()))]),
            )],
        ),
    ];
    for (what, kind, opts) in cases {
        match build_spec(kind, opts) {
            Err(WidgetError::InvalidOption { kind: k, .. }) => {
                assert_eq!(&k, kind, "{what}: error names the wrong widget");
            }
            other => panic!("{what}: expected InvalidOption, got {other:?}"),
        }
    }
}

#[test]
fn unknown_kind_returns_unknown_kind_error() {
    match build_spec("not-a-real-widget", &[]) {
        Err(WidgetError::UnknownKind(k)) => assert_eq!(k, "not-a-real-widget"),
        other => panic!("expected UnknownKind, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// WidgetCells helpers
// ---------------------------------------------------------------------------

#[test]
fn widget_cells_from_text_one_cell_per_char() {
    let cells = WidgetCells::from_text("hi");
    assert_eq!(cells.len(), 2);
    assert!(!cells.is_empty());

    let empty = WidgetCells::from_text("");
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
}

// ---------------------------------------------------------------------------
// windows (tab-bar) widget
// ---------------------------------------------------------------------------

/// Table-driven tab rendering with the default format/separator:
/// plain tabs, the ` Z` zoom suffix on a zoomed window (phux-x2hm), the
/// ` !` attention suffix (phux-foz.1, ADR-0035), and the empty list.
#[test]
fn windows_widget_renders_tabs_and_markers() {
    let cases: &[(&str, Vec<WindowInfo>, &str)] = &[
        (
            "default format and separator",
            vec![win("a", true), win("b", false)],
            "0:a 1:b",
        ),
        (
            "zoomed active window gets tmux's Z suffix; others unmarked",
            vec![
                WindowInfo {
                    zoomed: true,
                    ..win("a", true)
                },
                win("b", false),
            ],
            "0:a Z 1:b",
        ),
        (
            "attention window gets the ! suffix; others stay plain",
            vec![
                win("a", true),
                WindowInfo {
                    attention: true,
                    ..win("b", false)
                },
            ],
            "0:a 1:b !",
        ),
        ("empty list renders nothing", vec![], ""),
    ];
    for (what, windows, want) in cases {
        let cells = render_windows(&[], windows);
        assert_eq!(&text_of(&cells), want, "{what}");
        if want.is_empty() {
            assert!(cells.is_empty(), "{what}");
        }
    }
}

#[test]
fn help_hints_widget_uses_configured_prefix() {
    let widget = build_spec("help-hints", &[]).expect("help-hints builds");
    let cells = widget.render(&WidgetContext::new(fixed_time(), "", "C-b", &[]));

    assert_eq!(text_of(&cells), "C-b ? help | C-b : palette | C-b [ copy");
}

#[test]
fn windows_widget_stamps_hit_targets_on_tab_cells() {
    // phux-foz.12: every cell of a tab segment carries its window's hit
    // target (markers included); separator cells are inert. Cell-for-cell
    // against the default format "0:a 1:b Z" (window 1 zoomed... use the
    // attention marker on 1 to cover marker cells too).
    let cells = render_windows(
        &[],
        &[
            win("a", true),
            WindowInfo {
                attention: true,
                ..win("bee", false)
            },
        ],
    );
    // "0:a 1:bee !" — columns 0..3 → window 0, column 3 separator, 4..11 → window 1.
    assert_eq!(text_of(&cells), "0:a 1:bee !");
    for (i, cell) in cells.cells.iter().enumerate() {
        let expected = match i {
            0..=2 => Some(CellHit::Window(0)),
            3 => None, // separator
            _ => Some(CellHit::Window(1)),
        };
        assert_eq!(cell.hit, expected, "cell {i} ({:?})", cell.text);
    }
}

#[test]
fn windows_widget_stamps_hits_with_custom_format_and_separator() {
    // phux-foz.12: hit stamping follows the rendered segments, not the
    // default template — a custom format/separator keeps targets aligned.
    let cells = render_windows(
        &[
            ("format", toml::Value::String("{name}".to_owned())),
            ("separator", toml::Value::String(" | ".to_owned())),
        ],
        &[win("edit", true), win("logs", false)],
    );
    assert_eq!(text_of(&cells), "edit | logs");
    let hits: Vec<Option<CellHit>> = cells.cells.iter().map(|c| c.hit).collect();
    let w = |i: usize| Some(CellHit::Window(i));
    assert_eq!(
        hits,
        vec![
            w(0),
            w(0),
            w(0),
            w(0), // "edit"
            None,
            None,
            None, // " | "
            w(1),
            w(1),
            w(1),
            w(1), // "logs"
        ]
    );
}

#[test]
fn non_windows_widgets_produce_inert_cells() {
    // phux-foz.12: only the windows widget stamps hit targets — a click on
    // any other widget's cells must be a no-op.
    let w = SessionNameWidget::new(None, None);
    let cells = w.render(&WidgetContext::new(fixed_time(), "main", "C-a", &[]));
    assert!(cells.cells.iter().all(|c| c.hit.is_none()));
}

#[test]
fn windows_widget_active_and_inactive_styles_differ() {
    // Default preset: active = bold+reverse, inactive = dim.
    let cells = render_windows(&[], &[win("a", true), win("b", false)]);
    // First cell ("0") is part of the active segment.
    let active_style = cells.cells[0].style.clone().expect("active styled");
    assert!(active_style.bold && active_style.reverse);
    // The "b" cell belongs to the inactive segment "1:b" — find it.
    let b_cell = cells
        .cells
        .iter()
        .find(|c| c.text.first() == Some(&'b'))
        .expect("b cell");
    let inactive_style = b_cell.style.clone().expect("inactive styled");
    assert!(inactive_style.dim && !inactive_style.reverse);
}

#[test]
fn windows_widget_custom_style_parses() {
    let cells = render_windows(
        &[(
            "active",
            style_table(&[
                ("fg", toml::Value::String("green".to_owned())),
                ("bold", toml::Value::Boolean(true)),
            ]),
        )],
        &[win("a", true)],
    );
    let style = cells.cells[0].style.clone().expect("active styled");
    assert_eq!(style.fg.as_deref(), Some("green"));
    assert!(style.bold);
}

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
fn help_hints_rejects_any_opt() {
    let err = build_spec("help-hints", &[("anything", toml::Value::Boolean(true))])
        .expect_err("help-hints takes no options");
    match err {
        WidgetError::InvalidOption { kind, message } => {
            assert_eq!(kind, "help-hints");
            assert!(message.contains("unknown option `anything`"), "{message}");
        }
        other @ WidgetError::UnknownKind(_) => panic!("expected InvalidOption, got {other:?}"),
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

// ---------------------------------------------------------------------------
// Universal widget-level `style` (phux-i0e8.4.2)
// ---------------------------------------------------------------------------

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

#[test]
fn cell_style_is_plain_detects_default() {
    assert!(CellStyle::default().is_plain());
    assert!(
        !CellStyle {
            bold: true,
            ..CellStyle::default()
        }
        .is_plain()
    );
}
