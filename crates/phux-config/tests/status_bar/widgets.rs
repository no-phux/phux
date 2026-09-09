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
    let ctx = WidgetContext::new(fixed_time(), "", "C-b", &[]);

    // The prefix is stated once and the hints are its continuations.
    assert_eq!(
        text_of(&widget.render(&ctx)),
        "C-b  Space palette · ? help · [ copy"
    );
}

/// The hints exist to be read by someone who does not know the keys yet,
/// so a narrowing bar drops whole hints rather than clipping one into
/// nonsense — and renders nothing at all before it renders a fragment.
#[test]
fn help_hints_widget_drops_whole_hints_as_it_narrows() {
    let widget = build_spec("help-hints", &[]).expect("help-hints builds");
    let ctx = WidgetContext::new(fixed_time(), "", "C-b", &[]);
    let at = |budget| text_of(&widget.render_within(&ctx, budget));

    assert_eq!(at(36), "C-b  Space palette · ? help · [ copy");
    assert_eq!(at(35), "C-b  Space palette · ? help");
    assert_eq!(at(26), "C-b  Space palette");
    assert_eq!(at(17), "");
    // No ellipsis ever appears in this widget's output.
    for budget in 0..40 {
        assert!(!at(budget).contains('…'), "clipped at budget {budget}");
    }
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

// ---------------------------------------------------------------------------
// Width contract: cells are COLUMNS (phux-l96p.8 fix pass II)
// ---------------------------------------------------------------------------

/// Walk a composed row exactly as the emitter does, returning the
/// columns it advances the terminal.
///
/// The emitter derives "claimed" from POSITION, not from the cell: after
/// writing a symbol it advances by that symbol's display width, so a
/// double-width character consumes the cell after it and a genuinely
/// blank cell is written as one space. `Err(index)` means the walk ran
/// off the end of the row — an ORPHAN BASE, a cell the row counts as one
/// column and the terminal advances two for.
fn emitted_columns(cells: &[phux_config::widget::Cell]) -> Result<usize, usize> {
    let mut i = 0usize;
    let mut columns = 0usize;
    while i < cells.len() {
        let w = phux_config::widget::cell_columns(&cells[i]).max(1);
        if i + w > cells.len() {
            return Err(i);
        }
        columns += w;
        i += w;
    }
    Ok(columns)
}

/// The separator is user-configurable TEXT, so it can be double-width.
/// `windowed_width` measured it with `chars().count()` while the cells
/// it was predicting came from the width-aware `from_styled`, so every
/// gap under-counted by a column: the widget handed back more cells than
/// its budget, tripping its own `debug_assert` in debug and test builds,
/// and in release leaving the slot clamp to cut the overrun — possibly
/// through the middle of a double-width character.
#[test]
fn a_wide_tab_separator_never_overruns_the_budget() {
    let windows = vec![win("a", true), win("b", false), win("c", false)];
    let opts = [(
        "separator",
        // U+FF5C FULLWIDTH VERTICAL LINE: one char, two columns.
        toml::Value::String("\u{ff5c}".to_owned()),
    )];
    let widget = build_spec("windows", &opts).expect("windows builds");
    let ctx = WidgetContext::new(fixed_time(), "", "C-a", &windows);
    // The natural strip is 3 tabs (3 columns each) and 2 separators (2
    // columns each) = 13 columns; the old char count said 11.
    let natural = widget.render(&ctx);
    assert_eq!(natural.len(), 13);
    assert_eq!(emitted_columns(&natural.cells), Ok(13));
    for budget in 0..20 {
        let out = widget.render_within(&ctx, budget);
        assert!(
            out.len() <= budget,
            "budget {budget}: widget returned {} cells",
            out.len()
        );
        // And `len()` really is the column count, which is the contract
        // every consumer of the strip relies on.
        assert_eq!(
            emitted_columns(&out.cells),
            Ok(out.len()),
            "budget {budget}: cells and emitted columns disagree"
        );
    }
}

/// A composed row must advance the terminal exactly as many columns as
/// it claims. An ORPHAN BASE — a double-width character whose claimed
/// cell was cut away — is one cell the row counts and two the terminal
/// advances; in the right slot it lands on the last column, so the row
/// runs past the end and wraps into the pane grid below (ADR-0020).
///
/// Swept across widths so the boundary is hit exactly, and with a
/// `spacer` in the bar so the ADR-0087 slack path is exercised at those
/// boundaries too.
#[test]
fn a_composed_row_advances_exactly_its_own_width() {
    use phux_config::widget::{StatusBar, WidgetRegistry};
    use phux_config::{StatusCfg, Widget, WidgetSpec};

    let windows = vec![win("\u{65e5}\u{672c}", true), win("b", false)];
    let cfg = StatusCfg {
        left: vec![Widget::Spec(WidgetSpec {
            kind: "windows".to_owned(),
            opts: opts_with(&[("separator", toml::Value::String("\u{ff5c}".to_owned()))]),
        })],
        center: vec![Widget::Bare("spacer".to_owned())],
        right: vec![Widget::Bare("session-name".to_owned())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
    for width in 1usize..48 {
        let ctx = WidgetContext::new(
            fixed_time(),
            "\u{65e5}\u{672c}\u{8a9e}session",
            "C-a",
            &windows,
        );
        let row = bar.render(&ctx, u16::try_from(width).unwrap());
        assert_eq!(row.len(), width, "width {width}: wrong cell count");
        assert_eq!(
            emitted_columns(&row),
            Ok(width),
            "width {width}: the row does not advance exactly its own width"
        );
    }
}

/// `clip` stamps its ellipsis on a BASE cell, never on a claimed one.
/// A claimed cell is skipped at emit — the terminal advanced past it
/// when it drew the base — so an ellipsis parked there is never drawn,
/// and a clipped strip silently loses the mark that says it was clipped.
#[test]
fn a_clip_marks_the_cut_even_through_a_wide_character() {
    // [a][\u{65e5}][claimed][b] — four cells, four columns.
    let full = WidgetCells::from_text("a\u{65e5}b");
    assert_eq!(full.len(), 4);
    for budget in 1..=4 {
        let out = full.clone().clipped(budget);
        assert!(out.len() <= budget, "budget {budget} overrun");
        assert_eq!(
            emitted_columns(&out.cells),
            Ok(out.len()),
            "budget {budget}: a clip left a base whose claimed cell was cut away"
        );
        if budget < 4 {
            assert!(
                out.cells
                    .iter()
                    .any(|c| c.text.first() == Some(&phux_config::widget::ELLIPSIS)),
                "budget {budget}: a clipped strip must carry its cut marker"
            );
        }
    }
    // The cut that lands between the wide base and its claimed cell
    // takes the whole character and marks the cut on the cell before it.
    assert_eq!(text_of(&full.clone().clipped(3)), "a\u{2026}");
    // A strip that fits is untouched, ellipsis included.
    assert_eq!(text_of(&full.clipped(4)), "a\u{65e5}b");
}

/// The slot clamp is the row's last line of defence — its stated job is
/// that "a third-party widget cannot corrupt the row geometry" — so it
/// has to survive a widget that both overruns AND ends on a double-width
/// character. Cutting between that character's base and the cell it
/// claimed leaves an ORPHAN BASE: one cell the row counts and two
/// columns the terminal advances. In the RIGHT slot the orphan lands on
/// the last column, the terminal advances past the end of the row, and
/// the bar wraps into the pane grid below (ADR-0020).
///
/// Swept so the cut lands on the boundary exactly, and with a `spacer`
/// so the ADR-0087 slack path is exercised at those boundaries too.
#[test]
fn the_slot_clamp_never_leaves_an_orphan_base() {
    use phux_config::widget::{StatusBar, WidgetRegistry};
    use phux_config::{StatusCfg, Widget};

    /// Renders "aa日本語..." and IGNORES its budget entirely.
    #[derive(Debug)]
    struct Greedy;
    impl StatusWidget for Greedy {
        fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
            WidgetCells::from_text("aa\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{5e45}")
        }
        fn render_within(&self, ctx: &WidgetContext<'_>, _budget: usize) -> WidgetCells {
            self.render(ctx)
        }
    }
    #[allow(clippy::unnecessary_wraps)]
    fn greedy(_opts: &BTreeMap<String, toml::Value>) -> Result<Box<dyn StatusWidget>, WidgetError> {
        Ok(Box::new(Greedy))
    }

    let mut reg = WidgetRegistry::with_builtins();
    reg.register("greedy", greedy);
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".to_owned())],
        center: vec![Widget::Bare("spacer".to_owned())],
        right: vec![Widget::Bare("greedy".to_owned())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &reg).expect("bar builds");
    for width in 1u16..24 {
        let ctx = WidgetContext::new(fixed_time(), "s", "C-a", &[]);
        let row = bar.render(&ctx, width);
        assert_eq!(row.len(), usize::from(width), "width {width}");
        assert_eq!(
            emitted_columns(&row),
            Ok(usize::from(width)),
            "width {width}: the row does not advance exactly its own width"
        );
    }
}

/// A window name is a pane's OSC-2 title, and the bar is emitted as raw
/// VT. Explicit bidi overrides are zero-width, so they cost no budget
/// and no width check notices them, but they reorder everything drawn
/// after them — a pane could make its own tab read as another window's.
#[test]
fn a_window_name_cannot_carry_a_bidi_override() {
    let cells = render_windows(&[], &[win("run\u{202e}gpj.exe", true)]);
    let text = text_of(&cells);
    assert!(
        !text.contains('\u{202e}'),
        "a bidi override reached the tab bar: {text:?}"
    );
    assert_eq!(text, "0:rungpj.exe");
    // Zero-width, so it changes no geometry either.
    assert_eq!(
        cells.len(),
        render_windows(&[], &[win("rungpj.exe", true)]).len()
    );
}

/// A budget too narrow for the strip's first character still says the
/// strip was cut. Dropping the whole character and then having nothing
/// to stamp the ellipsis on would leave a one-column strip rendering
/// blank — indistinguishable from a widget that had nothing to say.
#[test]
fn a_clip_narrower_than_the_first_character_still_marks_the_cut() {
    let wide = WidgetCells::from_text("\u{65e5}\u{672c}");
    assert_eq!(wide.len(), 4);
    let out = wide.clipped(1);
    assert_eq!(text_of(&out), "\u{2026}");
    assert_eq!(out.len(), 1);
    assert_eq!(emitted_columns(&out.cells), Ok(1));
    // Zero budget really is empty, though: there is no column to mark.
    assert!(WidgetCells::from_text("\u{65e5}").clipped(0).is_empty());
}
