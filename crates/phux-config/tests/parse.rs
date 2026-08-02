//! Integration tests for the config schema.
//!
//! Covers:
//! 1. The canonical `docs/consumers/tui.md` §4.2 example round-trips
//!    (`parse → serialize → reparse` is equal under `PartialEq`).
//! 2. A syntactically-malformed input produces a `ConfigError::Parse`
//!    with the expected `line:col`, and we snapshot its `Display`.
//! 3. Missing optional sections fall back to defaults.
//! 4. Unknown fields are rejected (`deny_unknown_fields`).

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

    // Spot-check a couple of fields so a regression doesn't silently
    // pass via two-way equality of broken values.
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
    // Only [defaults] present, and only one field within it. Everything
    // else must populate from `Default`.
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

/// Empty input is exactly `Config::default()`, AND the shipped default
/// values themselves are pinned per field so a change to any schema
/// default cannot slip through the two-way equality:
/// - which-key on with a 600 ms hesitation delay (phux-foz.2);
/// - predictive-echo OFF (phux-pxaj, re-evaluated phux-51n6.1: readline
///   vi command-mode and no-echo prompts remain un-gatable client-side,
///   and mosh's RTT-adaptive gating is not yet ported — opt in with
///   `predictive-echo = true`);
/// - sidebar disabled, width 20, on the left (phux-4h5a);
/// - status bar at the bottom (phux-foz.8);
/// - spawn knobs at their shipped values (phux-4li.1);
/// - `defaults.term` = xterm-256color (phux-ign): a regression here
///   silently changes the TERM advertised to every server-spawned pane;
/// - window-size `smallest`, which never crops content (ADR-0027).
#[test]
fn empty_input_is_full_defaults() {
    let cfg = parse_str("", &path()).expect("empty parses");
    assert_eq!(cfg, Config::default());

    assert!(cfg.keybindings.which_key);
    assert_eq!(cfg.keybindings.which_key_delay_ms, 600);
    assert!(!cfg.experimental.predictive_echo);
    assert!(!cfg.sidebar.enabled, "sidebar is off by default");
    assert_eq!(cfg.sidebar.width, 20, "default width");
    assert_eq!(cfg.sidebar.position, SidebarPosition::Left);
    assert_eq!(cfg.status.position, StatusPosition::Bottom);
    assert_eq!(cfg.defaults.cwd_inheritance, CwdInheritance::InheritFocused);
    assert_eq!(cfg.defaults.spawn_on_attach, None);
    assert_eq!(cfg.defaults.session_name_template, "default");
    assert_eq!(cfg.defaults.term, "xterm-256color");
    assert_eq!(cfg.defaults.window_size, WindowSize::Smallest);
    assert_eq!(WindowSize::default(), WindowSize::Smallest);

    // An empty [experimental] table is also valid and yields the same
    // default.
    let cfg2 = parse_str("[experimental]\n", &path()).expect("empty section parses");
    assert!(!cfg2.experimental.predictive_echo);
}

#[test]
fn which_key_keys_parse_under_keybindings() {
    let input = r"
[keybindings]
which-key = false
which-key-delay-ms = 250
";
    let cfg = parse_str(input, &path()).expect("which-key keys parse");
    assert!(!cfg.keybindings.which_key);
    assert_eq!(cfg.keybindings.which_key_delay_ms, 250);
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
    // Unclosed string in the middle of the prefix-table. The offending
    // token sits on the line with the bad value.
    //
    // Line layout (1-indexed):
    //   1: (empty leading newline)
    //   2: [keybindings.prefix-table]
    //   3: "c" = "kill-pane
    //   4: "x" = "kill-pane"
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

    // The error must point inside the broken line (line 3) — not at the
    // start of the file. We assert the line and a generous col window
    // so the test isn't brittle against `toml` crate minor bumps.
    assert_eq!(*line, 3, "error should point at the broken line");
    assert!(*col >= 1, "col must be 1-indexed");

    // Snapshot the Display form. Normalize the column to a placeholder
    // because exact column depends on `toml`'s internal pointer choice
    // (start of token vs. error position) and is allowed to drift.
    let rendered = format!("{err}");
    let normalized = normalize_col(&rendered);
    insta::assert_snapshot!("malformed_parse_error", normalized);
}

#[test]
fn spanless_schema_error_renders_no_fabricated_position() {
    // phux-i0e8.3.5: deserializing the merged layer stack yields errors
    // with no span into the user's text. Those used to render a
    // fabricated `1:1`; they must now carry no position at all.
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

// ---------------------------------------------------------------------------
// [experimental] predictive-echo  (phux-9gw.1.2)
// ---------------------------------------------------------------------------

#[test]
fn experimental_predictive_echo_parses_both_values() {
    let cfg = parse_str("[experimental]\npredictive-echo = true\n", &path())
        .expect("[experimental] section parses");
    assert!(
        cfg.experimental.predictive_echo,
        "predictive-echo = true should land as true in the typed view"
    );

    // The opt-out must stick: an explicit `false` parses as false.
    let cfg = parse_str("[experimental]\npredictive-echo = false\n", &path())
        .expect("[experimental] section parses");
    assert!(
        !cfg.experimental.predictive_echo,
        "predictive-echo = false should land as false in the typed view"
    );
}

#[test]
fn experimental_predictive_echo_malformed_value_reports_key() {
    // Bool field given an integer: the error must reach the user with
    // enough context to find the key.
    let input = r"
[experimental]
predictive-echo = 1
";
    let err = parse_str(input, &path()).expect_err("integer is not a bool");
    let ConfigError::Parse {
        message, position, ..
    } = err
    else {
        panic!("expected ConfigError::Parse for malformed value");
    };
    assert!(
        message.contains("bool") || message.contains("boolean"),
        "error should mention the expected type; got: {message}"
    );
    // The offending value sits on line 3 (leading newline + section line + value line).
    assert_eq!(
        position.map(|(line, _)| line),
        Some(3),
        "error should point at the broken value line"
    );
}

// ---------------------------------------------------------------------------
// Per-field user values parse and round-trip: [sidebar] (phux-4h5a),
// [status] position (phux-foz.8), defaults.term (phux-ign), the spawn
// knobs (phux-4li.1), and window-size (ADR-0027).
// ---------------------------------------------------------------------------

#[test]
fn user_values_parse_and_round_trip() {
    let cfg =
        parse_and_round_trip("[sidebar]\nenabled  = true\nwidth    = 30\nposition = \"right\"\n");
    assert!(cfg.sidebar.enabled);
    assert_eq!(cfg.sidebar.width, 30);
    assert_eq!(cfg.sidebar.position, SidebarPosition::Right);

    let cfg = parse_and_round_trip("[status]\nleft     = [\"session-name\"]\nposition = \"top\"\n");
    assert_eq!(cfg.status.position, StatusPosition::Top);

    // phux-ign: a user can opt into ghostty's extended terminfo by
    // setting `defaults.term`.
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

#[test]
fn cwd_inheritance_accepts_all_variants() {
    for (toml_value, expected) in [
        ("inherit-focused", CwdInheritance::InheritFocused),
        ("home", CwdInheritance::Home),
        ("session-root", CwdInheritance::SessionRoot),
        ("last-cwd-per-window", CwdInheritance::LastCwdPerWindow),
    ] {
        let input = format!("[defaults]\ncwd-inheritance = \"{toml_value}\"\n");
        let cfg = parse_str(&input, &path())
            .unwrap_or_else(|e| panic!("variant {toml_value} should parse: {e}"));
        assert_eq!(cfg.defaults.cwd_inheritance, expected);
    }
}

#[test]
fn window_size_accepts_all_variants() {
    for (toml_value, expected) in [
        ("smallest", WindowSize::Smallest),
        ("largest", WindowSize::Largest),
        ("latest", WindowSize::Latest),
        ("manual", WindowSize::Manual),
    ] {
        let input = format!("[defaults]\nwindow-size = \"{toml_value}\"\n");
        let cfg = parse_str(&input, &path())
            .unwrap_or_else(|e| panic!("variant {toml_value} should parse: {e}"));
        assert_eq!(cfg.defaults.window_size, expected);
    }
}

#[test]
fn embedded_default_toml_populates_new_knobs() {
    // The shipped `default.toml` (via `parse_with_defaults`) must
    // populate the new knobs at their documented defaults.
    let cfg = phux_config::parse_with_defaults("", &path()).expect("embedded defaults parse");
    assert_eq!(cfg.defaults.cwd_inheritance, CwdInheritance::InheritFocused);
    assert_eq!(cfg.defaults.spawn_on_attach, None);
    assert_eq!(cfg.defaults.session_name_template, "default");
    assert_eq!(cfg.defaults.window_size, WindowSize::Smallest);
    // history-limit is the canonical scrollback knob (phux-4li.1 DEDUPE).
    assert_eq!(cfg.defaults.history_limit, 50_000);
    assert!(matches!(
        cfg.status.center.as_slice(),
        [phux_config::Widget::Spec(spec)] if spec.kind == "help-hints"
    ));
}

#[test]
fn user_can_override_one_new_knob_without_restating_others() {
    // Layered parse: setting only `cwd-inheritance` must leave the other
    // new knobs at their embedded-default values.
    let user = r#"
[defaults]
cwd-inheritance = "session-root"
"#;
    let cfg = phux_config::parse_with_defaults(user, &path()).expect("partial override parses");
    assert_eq!(cfg.defaults.cwd_inheritance, CwdInheritance::SessionRoot);
    assert_eq!(cfg.defaults.spawn_on_attach, None);
    assert_eq!(cfg.defaults.session_name_template, "default");
    assert_eq!(cfg.defaults.window_size, WindowSize::Smallest);
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
