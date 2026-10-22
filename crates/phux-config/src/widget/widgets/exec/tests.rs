use super::*;
use crate::schema::WidgetSpec;
use crate::widget::WidgetRegistry;
use std::time::UNIX_EPOCH;

fn build(opts: &[(&str, toml::Value)]) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let spec = WidgetSpec {
        kind: "exec".to_owned(),
        opts: opts
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    };
    WidgetRegistry::with_builtins().build(&spec)
}

fn text_of(cells: &WidgetCells) -> String {
    cells.cells.iter().filter_map(|c| c.text.first()).collect()
}

fn ctx() -> WidgetContext<'static> {
    WidgetContext::new(UNIX_EPOCH, "", "C-a", &[])
}

#[test]
fn string_command_runs_via_sh_and_array_is_argv() {
    let w = build(&[("command", "echo hi".into())]).unwrap();
    let feed = w.exec_feed().expect("exec widget exposes a feed");
    assert_eq!(feed.argv(), ["/bin/sh", "-c", "echo hi"]);
    assert_eq!(feed.interval(), Duration::from_secs(5), "doc default 5s");

    let w = build(&[("command", vec!["battery", "--percent"].into())]).unwrap();
    assert_eq!(w.exec_feed().unwrap().argv(), ["battery", "--percent"]);
}

#[test]
fn interval_parses_units_and_floors_to_one_second() {
    for (raw, want) in [
        ("30s", Duration::from_secs(30)),
        ("2m", Duration::from_secs(120)),
        ("1h", Duration::from_secs(3600)),
        ("7", Duration::from_secs(7)),
        // Sub-second cadences floor to the 1s repaint tick.
        ("200ms", Duration::from_secs(1)),
    ] {
        let w = build(&[
            ("command", toml::Value::String("true".to_owned())),
            ("interval", toml::Value::String(raw.to_owned())),
        ])
        .unwrap();
        assert_eq!(w.exec_feed().unwrap().interval(), want, "interval {raw:?}");
        assert_eq!(w.poll_interval(), Some(want));
    }
}

#[test]
fn invalid_options_are_rejected() {
    for opts in [
        vec![],                                                  // no command
        vec![("command", toml::Value::String("  ".to_owned()))], // blank
        vec![("command", toml::Value::Array(vec![]))],           // empty argv
        vec![("command", toml::Value::Integer(3))],              // wrong type
        vec![
            ("command", toml::Value::String("true".to_owned())),
            ("interval", toml::Value::String("fast".to_owned())),
        ],
        vec![
            ("command", toml::Value::String("true".to_owned())),
            ("interval", toml::Value::String("0s".to_owned())),
        ],
        vec![
            ("command", toml::Value::String("true".to_owned())),
            ("parse-ansi", toml::Value::String("yes".to_owned())),
        ],
    ] {
        assert!(
            matches!(build(&opts), Err(WidgetError::InvalidOption { .. })),
            "opts {opts:?} should be rejected"
        );
    }
}

#[test]
fn render_is_empty_until_output_arrives_then_shows_cached_line() {
    let w = build(&[("command", toml::Value::String("true".to_owned()))]).unwrap();
    assert!(w.render(&ctx()).is_empty(), "no run yet -> empty strip");
    let feed = w.exec_feed().unwrap();
    feed.apply_output("BAT 87%\nsecond line ignored\n");
    assert_eq!(text_of(&w.render(&ctx())), "BAT 87%");
    // A later run replaces the cache.
    feed.apply_output("BAT 86%\n");
    assert_eq!(text_of(&w.render(&ctx())), "BAT 86%");
}

#[test]
fn parse_ansi_styles_cells_from_sgr() {
    let cells = parse_ansi_line("\u{1b}[1;31mA\u{1b}[0mB");
    assert_eq!(text_of(&cells), "AB");
    let a = cells.cells[0].style.clone().expect("A styled");
    assert!(a.bold);
    assert_eq!(a.fg.as_deref(), Some("1"));
    assert!(cells.cells[1].style.is_none(), "B reset to plain");

    let cells = parse_ansi_line("\u{1b}[38;5;208mX\u{1b}[48;2;16;32;48mY");
    let y = cells.cells[1].style.as_ref().unwrap();
    assert_eq!(y.bg.as_deref(), Some("#102030"));
    assert_eq!(y.fg.as_deref(), Some("208"), "fg persists across cells");

    let cells = parse_ansi_line("\u{1b}[97;100mZ\u{1b}[39;49mQ");
    let z = cells.cells[0].style.as_ref().unwrap();
    assert_eq!(z.fg.as_deref(), Some("15"), "97 = bright white = idx 15");
    assert_eq!(z.bg.as_deref(), Some("8"), "100 = bright black bg = idx 8");
    assert!(cells.cells[1].style.is_none(), "39;49 clears both colors");
}

#[test]
fn parse_ansi_strips_non_sgr_escapes_and_controls() {
    // Cursor-move CSI, an OSC title, a two-char escape, and a tab all
    // strip; printable text survives.
    let cells = parse_ansi_line("\u{1b}[2Ka\u{1b}]0;title\u{7}b\u{1b}(Bc\td");
    assert_eq!(text_of(&cells), "abcd");
    assert!(cells.cells.iter().all(|c| c.style.is_none()));
}

#[test]
fn parse_ansi_handles_st_terminated_osc_and_unterminated_csi() {
    // An OSC ends at ST (ESC \); an ESC followed by anything else inside
    // the OSC body does not end it.
    let cells = parse_ansi_line("a\u{1b}]8;;x\u{1b}yz\u{1b}\\b");
    assert_eq!(text_of(&cells), "ab");
    // A CSI with no final byte swallows the rest of the line.
    let cells = parse_ansi_line("\u{1b}[1mA\u{1b}[31");
    assert_eq!(text_of(&cells), "A");
    assert!(cells.cells[0].style.as_ref().unwrap().bold);
    // A trailing ESC or charset designation at end of line is harmless.
    assert_eq!(text_of(&parse_ansi_line("x\u{1b}")), "x");
    assert_eq!(text_of(&parse_ansi_line("x\u{1b}(")), "x");
    assert_eq!(text_of(&parse_ansi_line("x\u{1b}(By")), "xy");
}

#[test]
fn parse_ansi_disabled_strips_sgr_instead_of_styling() {
    let w = build(&[
        ("command", toml::Value::String("true".to_owned())),
        ("parse-ansi", toml::Value::Boolean(false)),
    ])
    .unwrap();
    let feed = w.exec_feed().unwrap();
    feed.apply_output("\u{1b}[31mred\u{1b}[0m\n");
    let cells = w.render(&ctx());
    assert_eq!(text_of(&cells), "red");
    assert!(cells.cells.iter().all(|c| c.style.is_none()));
}

#[test]
fn status_bar_exec_feeds_walks_all_slots() {
    use crate::schema::{StatusCfg, StatusPosition, Widget};
    use crate::widget::StatusBar;
    let exec = |cmd: &str| {
        Widget::Spec(WidgetSpec {
            kind: "exec".to_owned(),
            opts: std::iter::once(("command".to_owned(), toml::Value::String(cmd.to_owned())))
                .collect(),
        })
    };
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".to_owned()), exec("left")],
        center: vec![exec("center")],
        right: vec![exec("right")],
        position: StatusPosition::default(),
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).unwrap();
    let feeds = bar.exec_feeds();
    let cmds: Vec<&str> = feeds.iter().map(|f| f.argv()[2].as_str()).collect();
    assert_eq!(cmds, ["left", "center", "right"]);
}
