use super::*;
use phux_config::keybind::{Feed, parse_chord};

/// Write `contents` as a config file inside a fresh temp dir and
/// return `(dir_guard, config_path)`.
fn config_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, contents).expect("write config");
    (dir, path)
}

fn parse(toml: &str) -> Config {
    phux_config::parse_with_defaults(toml, Path::new("test.toml")).expect("config parses")
}

#[test]
fn strict_load_applies_keybinding_and_theme_changes() {
    let (_dir, path) = config_file(
        r##"
        [keybindings.prefix-table]
        X = "kill-pane"

        [theme]
        accent = "#ff0000"
        "##,
    );
    let new = TuiSettings::load_strict(&path).expect("valid config reloads");
    let kb = new.keybindings.expect("snapshot present");
    assert!(
        kb.prefix_table.contains_key("X"),
        "reload must pick up the new prefix-table binding",
    );
    assert_eq!(
        new.theme.accent,
        ratatui::style::Color::Rgb(0xff, 0, 0),
        "reload must pick up the new theme accent",
    );
    // The resolver is built from the same snapshot: the new chord
    // resolves (prefix, then X) to the bound action.
    let mut resolver = new.resolver.expect("resolver present");
    let prefix = parse_chord(&kb.prefix).expect("prefix parses");
    assert_eq!(resolver.feed(prefix), Feed::Partial);
    match resolver.feed(parse_chord("X").expect("chord parses")) {
        Feed::Resolved(ra) => assert_eq!(ra.action, "kill-pane"),
        other => panic!("expected the reloaded binding to resolve, got {other:?}"),
    }
}

/// The row allocator's own default must track the schema default, or a
/// painter built without a live config (a test, a capture) would lay a
/// different line than the glass.
#[test]
fn agents_share_default_matches_the_schema() {
    assert_eq!(
        crate::render::chrome::sidebar::DEFAULT_AGENTS_SHARE,
        phux_config::Config::default().sidebar.agents_share
    );
}

#[test]
fn strict_load_reads_which_key_and_sidebar_knobs() {
    let (_dir, path) = config_file(
        r#"
        [keybindings]
        which-key = false
        which-key-delay-ms = 250

        [sidebar]
        enabled = true
        width = 33
        position = "right"

        [defaults]
        mouse = false
        "#,
    );
    let new = TuiSettings::load_strict(&path).expect("valid config reloads");
    assert!(!new.which_key.enabled);
    assert_eq!(new.which_key.delay, Duration::from_millis(250));
    assert_eq!(
        new.sidebar,
        SidebarSettings {
            enabled: true,
            width: 33,
            edge: SidebarEdge::Right,
            agents_share: 50,
        }
    );
    assert!(!new.mouse_capture);
}

#[test]
fn failed_reload_keeps_previous_settings() {
    // Seed "previous" state clearly distinguishable from defaults.
    let mut settings = TuiSettings::tolerant_from(&parse(
        r"
        [keybindings]
        which-key-delay-ms = 123
        [sidebar]
        width = 41
        ",
    ));
    settings.theme = Theme {
        accent: ratatui::style::Color::Rgb(1, 2, 3),
        ..Theme::default()
    };
    settings.chrome = ChromeBreakpoints {
        compact_cols: 99,
        ..ChromeBreakpoints::DEFAULT
    };
    let old_theme = settings.theme;
    let old_chrome = settings.chrome;

    let (_dir, path) = config_file("this is [not valid toml");
    let err = settings
        .reload_in_place(&path)
        .expect_err("malformed TOML must fail the reload");
    assert!(!err.is_empty(), "error message must be surfaceable");

    // Every slot is exactly as it was: nothing half-applied.
    assert!(settings.keybindings.is_some());
    assert!(settings.resolver.is_some());
    assert_eq!(settings.theme, old_theme);
    assert_eq!(
        settings.chrome, old_chrome,
        "breakpoints must not be half-applied"
    );
    assert_eq!(settings.which_key.delay, Duration::from_millis(123));
    assert_eq!(settings.sidebar.width, 41);
}

/// Reloading a file that binds `c` to a misspelled action must not
/// replace the working `new-window` binding with one that does nothing:
/// the reload is refused, the toast names the binding and the fix, and
/// the previous bindings stay.
#[test]
fn reload_refuses_an_unknown_action_and_keeps_the_working_binding() {
    let mut settings = TuiSettings::tolerant_from(&parse(""));
    let (_dir, path) = config_file(
        r#"
        [keybindings.prefix-table]
        c = "new-windoww"
        "#,
    );
    let err = settings
        .reload_in_place(&path)
        .expect_err("an unknown action must fail the reload");
    assert_eq!(
        err,
        "keybinding \"c\": unknown action `new-windoww` (did you mean `new-window`?)"
    );
    let mut resolver = settings.resolver.expect("previous resolver kept");
    assert_eq!(
        resolver.feed(parse_chord("C-a").expect("prefix parses")),
        Feed::Partial
    );
    match resolver.feed(parse_chord("c").expect("chord parses")) {
        Feed::Resolved(action) => assert_eq!(action.action, "new-window"),
        other => panic!("the shipped binding must survive, got {other:?}"),
    }
}

#[test]
fn strict_build_rejects_a_bad_binding_that_the_tolerant_build_degrades() {
    let cfg = parse(
        r#"
        [keybindings.prefix-table]
        "q-" = "kill-pane"
        d = "detach"
        "#,
    );
    let err = TuiSettings::strict_from(&cfg).expect_err("strict rejects the malformed chord");
    assert!(!err.is_empty());

    let tolerant = TuiSettings::tolerant_from(&cfg);
    // The lenient resolver survives, and the bar row names the chord.
    let mut resolver = tolerant.resolver.expect("lenient resolver present");
    let prefix = parse_chord("C-a").expect("prefix parses");
    assert_eq!(resolver.feed(prefix), Feed::Partial);
    assert!(matches!(
        resolver.feed(parse_chord("d").expect("chord parses")),
        Feed::Resolved(_)
    ));
    let bar = tolerant.status_bar.expect("error-line painter present");
    assert!(bar.is_error_line(), "the diagnostic takes the bar row");
}

#[test]
fn adopt_reload_leaves_attach_time_settings_alone() {
    let mut settings = TuiSettings::tolerant_from(&parse(
        r"
        [sidebar]
        width = 41
        [defaults]
        mouse = false
        ",
    ));
    let new = TuiSettings::strict_from(&parse(
        r##"
        [sidebar]
        width = 12
        [defaults]
        mouse = true
        [theme]
        accent = "#010203"
        "##,
    ))
    .expect("valid");
    settings.adopt_reload(new);
    assert_eq!(settings.theme.accent, ratatui::style::Color::Rgb(1, 2, 3));
    assert_eq!(
        settings.sidebar.width, 41,
        "sidebar geometry is attach-time"
    );
    assert!(!settings.mouse_capture, "mouse capture is attach-time");
}

/// A config that does not load at all (here a TOML syntax error and an
/// unknown key) must still leave the shipped keys working, so the user can
/// at least detach, with the load error held on the bar row.
#[test]
fn a_config_that_does_not_load_keeps_the_shipped_keys_and_shows_the_error() {
    for broken in ["this is [not valid toml", "[sidebar]\nenabledd = true\n"] {
        let (_dir, path) = config_file(broken);
        let err = phux_config::loader::load_from(&path).expect_err("the file must not load");
        let settings = TuiSettings::without_config(&err);

        let kb = settings.keybindings.expect("shipped bindings present");
        assert_eq!(kb.prefix, "C-a", "the shipped prefix survives {broken:?}");
        let mut resolver = settings.resolver.expect("shipped resolver present");
        assert_eq!(
            resolver.feed(parse_chord("C-a").expect("prefix parses")),
            Feed::Partial
        );
        match resolver.feed(parse_chord("d").expect("chord parses")) {
            Feed::Resolved(action) => assert_eq!(action.action, "detach"),
            other => panic!("C-a d must detach on a broken config, got {other:?}"),
        }
        let bar = settings.status_bar.expect("error line present");
        assert!(bar.is_error_line(), "the load error takes the bar row");
        assert!(settings.mouse_capture);
    }
}
