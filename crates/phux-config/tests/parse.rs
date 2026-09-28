//! Integration tests for the config schema: round trips, defaults,
//! rejection of unknown keys, and error positions.

use std::path::PathBuf;

use phux_config::{
    Config, ConfigError, CwdInheritance, DefaultsCfg, SidebarPosition, StatusPosition, WindowSize,
    parse_str,
};

mod common;
use common::path;

/// The canonical example from `docs/consumers/tui.md` §4.2.
const CANONICAL: &str = r##"
[defaults]
shell          = "/bin/zsh"
history-limit  = 50000

[keybindings]
prefix = "ctrl+space"

# Bindings under the prefix.
[keybindings.prefix-table]
"c"        = { action = "new-pane", direction = "horizontal" }
"v"        = { action = "new-pane", direction = "vertical" }
"x"        = "kill-pane"
"n"        = "new-window"
"tab"      = "next-window"
"h"        = { action = "focus-pane", direction = "left" }
"j"        = { action = "focus-pane", direction = "down" }
"k"        = { action = "focus-pane", direction = "up" }
"l"        = { action = "focus-pane", direction = "right" }
"d"        = "detach"
"shift+r"  = "rename-window"

# Global table: bindings that fire without a prefix.
[keybindings.global]

[status]
left   = ["session"]
center = ["windows"]
right  = [{ kind = "clock", format = "%H:%M" }]

[[hooks.pane-exit]]
when   = { exit-code = 0 }
action = "noop"

[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "notify", text = "pane {pane} exited with {exit-code}" }

[theme]
fg = "#cdd6f4"
bg = "#1e1e2e"
"##;

/// Parse `input`, then assert serialize → reparse is the identity.
#[allow(clippy::expect_used, reason = "test support")]
fn parse_and_round_trip(input: &str) -> Config {
    let cfg: Config = parse_str(input, &path()).expect("input parses");
    let reserialized = toml::to_string(&cfg).expect("re-serialize");
    let reparsed: Config =
        parse_str(&reserialized, &path()).expect("reparse of re-serialized config");
    assert_eq!(cfg, reparsed, "round trip should be identity");
    cfg
}

#[test]
fn canonical_example_round_trips() {
    let parsed = parse_and_round_trip(CANONICAL);

    assert_eq!(parsed.keybindings.prefix, "ctrl+space");
    assert_eq!(parsed.defaults.shell.as_deref(), Some("/bin/zsh"));
    assert_eq!(parsed.defaults.history_limit, 50_000);
    assert_eq!(parsed.hooks.get("pane-exit").map(Vec::len), Some(2));
    assert_eq!(
        parsed.theme.slots.get("fg").map(String::as_str),
        Some("#cdd6f4")
    );
}

#[test]
fn missing_sections_use_defaults() {
    let input = r#"
[defaults]
shell = "/bin/bash"
"#;
    let cfg = parse_str(input, &path()).expect("partial config parses");

    let want_defaults = DefaultsCfg {
        shell: Some("/bin/bash".to_owned()),
        ..DefaultsCfg::default()
    };
    assert_eq!(cfg.defaults, want_defaults);
    assert_eq!(cfg.keybindings.prefix, "C-a"); // schema default
    assert!(cfg.keybindings.prefix_table.is_empty());
    assert!(cfg.status.left.is_empty());
    assert!(cfg.hooks.is_empty());
    assert!(cfg.theme.slots.is_empty());
}

/// Empty input is exactly `Config::default()`, and the shipped values are
/// pinned per field so a changed default cannot slip through equality.
#[test]
fn empty_input_is_full_defaults() {
    let cfg = parse_str("", &path()).expect("empty parses");
    assert_eq!(cfg, Config::default());

    assert!(cfg.keybindings.which_key);
    assert_eq!(cfg.keybindings.which_key_delay_ms, 400);
    assert_eq!(cfg.experimental.predictive_echo, None);
    assert!(!cfg.experimental.predictive_echo_for(false));
    assert!(cfg.experimental.predictive_echo_for(true));
    assert!(cfg.sidebar.enabled);
    assert_eq!(cfg.sidebar.width, 0);
    assert_eq!(cfg.sidebar.position, SidebarPosition::Left);
    assert_eq!(cfg.status.position, StatusPosition::Top);
    assert_eq!(cfg.defaults.cwd_inheritance, CwdInheritance::InheritFocused);
    assert_eq!(cfg.defaults.spawn_on_attach, None);
    assert_eq!(cfg.defaults.session_name_template, "${cwd-basename}");
    assert_eq!(cfg.defaults.term, "xterm-256color");
    assert_eq!(cfg.defaults.window_size, WindowSize::Smallest);
    assert_eq!(cfg.defaults.history_limit, 50_000);
}

/// Table-driven rejection: unknown fields (`deny_unknown_fields`) and
/// unknown enum variants must all fail with `ConfigError::Parse`. Rows
/// with substrings additionally pin the message contents (any-of).
#[test]
fn unknown_fields_and_variants_are_rejected() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "unknown top-level field",
            "not-a-real-section = \"oops\"\n",
            &[],
        ),
        (
            "typo in [defaults]",
            "[defaults]\nshell = \"/bin/zsh\"\nhistroy-limit = 50000  # typo: histroy\n",
            &["histroy-limit", "unknown"],
        ),
        (
            "unknown sidebar position",
            "[sidebar]\nposition = \"floating\"\n",
            &[],
        ),
        ("typo in [sidebar]", "[sidebar]\nwdith = 20\n", &[]),
        (
            "unknown status position",
            "[status]\nposition = \"floating\"\n",
            &[],
        ),
        (
            "unknown cwd-inheritance variant",
            "[defaults]\ncwd-inheritance = \"random-walk\"\n",
            &[],
        ),
        (
            "unknown window-size variant",
            "[defaults]\nwindow-size = \"fit-to-content\"\n",
            &[],
        ),
    ];
    for (what, input, want_any) in cases {
        let err = parse_str(input, &path()).expect_err(&format!("{what}: input must be rejected"));
        let ConfigError::Parse { message, .. } = &err else {
            panic!("{what}: expected Parse variant, got {err:?}");
        };
        assert!(
            want_any.is_empty() || want_any.iter().any(|needle| message.contains(needle)),
            "{what}: message should mention one of {want_any:?}: {message}"
        );
    }
}

#[test]
fn malformed_input_reports_line_col_and_snapshots() {
    // An unclosed string on line 3.
    let input = "\n[keybindings.prefix-table]\n\"c\" = \"kill-pane\n\"x\" = \"kill-pane\"\n";

    let err =
        parse_str(input, &PathBuf::from("config.toml")).expect_err("malformed input should error");

    let ConfigError::Parse {
        position: Some((line, col)),
        ..
    } = &err
    else {
        panic!("expected Parse variant with a position, got {err:?}");
    };

    assert_eq!(*line, 3, "error should point at the broken line");
    assert!(*col >= 1, "col must be 1-indexed");

    // The column may drift across `toml` versions.
    let rendered = format!("{err}");
    let normalized = normalize_col(&rendered);
    insta::assert_snapshot!("malformed_parse_error", normalized);
}

#[test]
fn spanless_schema_error_renders_no_fabricated_position() {
    // Merged-stack deserialize errors have no span into the user's text.
    let input = "[defaults]\nhistory-limit = \"not a number\"\n";
    let err = phux_config::parse_with_defaults(input, &path())
        .expect_err("string is not a valid history-limit");
    let ConfigError::Parse { position, .. } = &err else {
        panic!("expected Parse variant, got {err:?}");
    };
    assert_eq!(
        *position, None,
        "merged-stack deserialize errors carry no span; got {position:?}"
    );
    let rendered = format!("{err}");
    assert!(
        !rendered.contains(":1:1"),
        "spanless error must not fabricate a 1:1 position: {rendered}"
    );
    assert!(
        rendered.starts_with("config.toml: "),
        "spanless error still names the file: {rendered}"
    );
}

/// Per-field user values parse and round-trip; explicit predictive echo
/// beats the per-transport default in both directions.
#[test]
fn user_values_parse_and_round_trip() {
    let cfg = parse_and_round_trip("[keybindings]\nwhich-key = false\nwhich-key-delay-ms = 250\n");
    assert!(!cfg.keybindings.which_key);
    assert_eq!(cfg.keybindings.which_key_delay_ms, 250);

    let cfg = parse_and_round_trip("[experimental]\npredictive-echo = true\n");
    assert!(cfg.experimental.predictive_echo_for(false));
    let cfg = parse_and_round_trip("[experimental]\npredictive-echo = false\n");
    assert!(!cfg.experimental.predictive_echo_for(true));

    let cfg =
        parse_and_round_trip("[sidebar]\nenabled  = true\nwidth    = 30\nposition = \"right\"\n");
    assert!(cfg.sidebar.enabled);
    assert_eq!(cfg.sidebar.width, 30);
    assert_eq!(cfg.sidebar.position, SidebarPosition::Right);

    let cfg = parse_and_round_trip("[status]\nleft     = [\"session-name\"]\nposition = \"top\"\n");
    assert_eq!(cfg.status.position, StatusPosition::Top);

    let cfg = parse_and_round_trip("[defaults]\nterm = \"ghostty\"\n");
    assert_eq!(cfg.defaults.term, "ghostty");

    let cfg = parse_and_round_trip(
        r#"
[defaults]
cwd-inheritance       = "home"
spawn-on-attach       = "/usr/bin/tmux-like"
session-name-template = "phux-${cwd-basename}"
"#,
    );
    assert_eq!(cfg.defaults.cwd_inheritance, CwdInheritance::Home);
    assert_eq!(
        cfg.defaults.spawn_on_attach.as_deref(),
        Some("/usr/bin/tmux-like")
    );
    assert_eq!(cfg.defaults.session_name_template, "phux-${cwd-basename}");

    let cfg = parse_and_round_trip("[defaults]\nwindow-size = \"largest\"\n");
    assert_eq!(cfg.defaults.window_size, WindowSize::Largest);
}

/// Replace the `:COL:` in `path:LINE:COL: message` with `:<col>:` so
/// the snapshot is stable across `toml` crate minor versions.
fn normalize_col(s: &str) -> String {
    // Format is `path: line:col: message`. Find the second colon
    // after the line number and rewrite up to the next colon.
    let Some(first_colon) = s.find(':') else {
        return s.to_owned();
    };
    let after_path = &s[first_colon + 1..];
    let Some(line_end) = after_path.find(':') else {
        return s.to_owned();
    };
    let rest = &after_path[line_end + 1..];
    let Some(col_end) = rest.find(':') else {
        return s.to_owned();
    };
    let mut out = String::with_capacity(s.len());
    out.push_str(&s[..=first_colon]);
    out.push_str(&after_path[..=line_end]);
    out.push_str("<col>");
    out.push_str(&rest[col_end..]);
    out
}
