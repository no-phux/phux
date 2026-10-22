//! Per-kind rendering and option validation.

use super::*;

// --- session-name widget

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

// --- time widget

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

// --- Invalid options (per-kind); unknown kind

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

// --- windows (tab-bar) widget

/// Table-driven tab rendering with the default format/separator:
/// plain tabs, the ` Z` zoom suffix on a zoomed window (phux-x2hm), the
/// ` !` attention suffix (phux-foz.1, ADR-0035), the ` x` / ` xN`
/// retained-exit suffix (phux-fpgl.33, ADR-0124), and the empty list.
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
        (
            "retained window gets the x suffix plus exit status",
            vec![
                win("a", true),
                WindowInfo {
                    exited: Some("3".to_owned()),
                    ..win("b", false)
                },
            ],
            "0:a 1:b x3",
        ),
        (
            "signal death and unknown exit still carry the x marker",
            vec![
                WindowInfo {
                    exited: Some("sig9".to_owned()),
                    ..win("a", true)
                },
                WindowInfo {
                    exited: Some(String::new()),
                    ..win("b", false)
                },
            ],
            "0:a xsig9 1:b x",
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
fn help_hints_widget_stamps_each_destination_with_its_action() {
    use phux_config::widget::CellHit;

    let widget = build_spec("help-hints", &[]).expect("help-hints builds");
    let ctx = WidgetContext::new(fixed_time(), "", "C-b", &[]);
    let row = widget.render(&ctx);
    let actions: Vec<_> = row
        .cells
        .iter()
        .filter_map(|cell| match cell.hit {
            Some(CellHit::Action(action)) => Some(action),
            Some(CellHit::Window(_) | CellHit::Switch) | None => None,
        })
        .collect();

    for action in [
        "session-picker",
        "command-palette",
        "settings",
        "show-help",
        "copy-mode",
    ] {
        assert!(actions.contains(&action), "missing hit target for {action}");
    }
    assert_eq!(row.cells[0].hit, None, "the prefix label stays inert");
}

/// The hints exist to be read by someone who does not know the keys yet,
/// so a narrowing bar drops whole hints rather than clipping one into
/// nonsense — and renders nothing at all before it renders a fragment.
#[test]
fn help_hints_widget_drops_whole_hints_as_it_narrows() {
    let widget = build_spec("help-hints", &[]).expect("help-hints builds");
    let ctx = WidgetContext::new(fixed_time(), "", "C-b", &[]);
    let at = |budget| text_of(&widget.render_within(&ctx, budget));

    assert_eq!(
        at(63),
        "C-b  s Sessions · Space Commands · S Settings · ? Help · [ Copy"
    );
    assert_eq!(
        at(62),
        "C-b  s Sessions · Space Commands · S Settings · ? Help"
    );
    assert_eq!(at(53), "C-b  s Sessions · Space Commands · S Settings");
    assert_eq!(at(44), "C-b  s Sessions · Space Commands");
    assert_eq!(at(31), "C-b  s Sessions");
    assert_eq!(at(14), "");
    // No ellipsis ever appears in this widget's output.
    for budget in 0..70 {
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
