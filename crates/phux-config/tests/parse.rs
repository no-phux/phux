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

fn path() -> PathBuf {
    PathBuf::from("config.toml")
}

#[test]
fn canonical_example_round_trips() {
    let parsed: Config = parse_str(CANONICAL, &path()).expect("canonical parses");

    // Re-serialize and re-parse; the two `Config` values must compare equal.
    let reserialized = toml::to_string(&parsed).expect("re-serialize");
    let reparsed: Config =
        parse_str(&reserialized, &path()).expect("reparse of re-serialized config");

    assert_eq!(parsed, reparsed, "round trip should be identity");

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

#[test]
fn empty_input_is_full_defaults() {
    let cfg = parse_str("", &path()).expect("empty parses");
    assert_eq!(cfg, Config::default());
}

#[test]
fn which_key_defaults_on_with_600ms_delay() {
    // phux-foz.2: the popup ships enabled with a 600 ms hesitation delay.
    let cfg = parse_str("", &path()).expect("empty parses");
    assert!(cfg.keybindings.which_key);
    assert_eq!(cfg.keybindings.which_key_delay_ms, 600);
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

#[test]
fn unknown_field_at_top_level_is_rejected() {
    let input = r#"
not-a-real-section = "oops"
"#;
    let err = parse_str(input, &path()).expect_err("unknown field rejected");
    assert!(matches!(err, ConfigError::Parse { .. }));
}

#[test]
fn unknown_field_in_defaults_is_rejected() {
    let input = r#"
[defaults]
shell = "/bin/zsh"
histroy-limit = 50000  # typo: histroy
"#;
    let err = parse_str(input, &path()).expect_err("typo rejected by deny_unknown_fields");
    let ConfigError::Parse { message, .. } = err else {
        panic!("expected Parse variant");
    };
    assert!(
        message.contains("histroy-limit") || message.contains("unknown"),
        "message should mention the unknown field: {message}"
    );
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
fn experimental_predictive_echo_true_parses() {
    let input = r"
[experimental]
predictive-echo = true
";
    let cfg = parse_str(input, &path()).expect("[experimental] section parses");
    assert!(
        cfg.experimental.predictive_echo,
        "predictive-echo = true should land as true in the typed view"
    );
}

#[test]
fn experimental_predictive_echo_defaults_off_when_absent() {
    // No [experimental] section at all: the field defaults OFF (phux-pxaj,
    // re-evaluated phux-51n6.1). The client now proactively gates prediction
    // on the alternate screen (full-screen apps), but the default stays off
    // because two main-screen cases remain un-gatable client-side — readline
    // vi command-mode and no-echo password prompts — and the mosh mechanisms
    // that make on-by-default safe (RTT-adaptive gating + a prediction
    // display-timeout) are not yet ported. Opt in with `predictive-echo =
    // true`.
    let cfg = parse_str("", &path()).expect("empty parses");
    assert!(
        !cfg.experimental.predictive_echo,
        "absent [experimental] section must leave predictive-echo off by default"
    );

    // Empty [experimental] table is also valid and yields the same default.
    let cfg2 = parse_str("[experimental]\n", &path()).expect("empty section parses");
    assert!(!cfg2.experimental.predictive_echo);
}

#[test]
fn experimental_predictive_echo_false_parses() {
    // The opt-out must stick: an explicit `false` overrides the on-default.
    let input = r"
[experimental]
predictive-echo = false
";
    let cfg = parse_str(input, &path()).expect("[experimental] section parses");
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
// [sidebar]  (phux-4h5a)
// ---------------------------------------------------------------------------

#[test]
fn sidebar_defaults_to_disabled_when_absent() {
    let cfg = parse_str("", &path()).expect("empty parses");
    assert!(!cfg.sidebar.enabled, "sidebar is off by default");
    assert_eq!(cfg.sidebar.width, 20, "default width");
    assert_eq!(cfg.sidebar.position, SidebarPosition::Left);
}

#[test]
fn sidebar_enabled_parses_and_round_trips() {
    let input = r#"
[sidebar]
enabled  = true
width    = 30
position = "right"
"#;
    let cfg = parse_str(input, &path()).expect("[sidebar] parses");
    assert!(cfg.sidebar.enabled);
    assert_eq!(cfg.sidebar.width, 30);
    assert_eq!(cfg.sidebar.position, SidebarPosition::Right);

    let reser = toml::to_string(&cfg).expect("reserialize");
    let reparsed = parse_str(&reser, &path()).expect("reparse");
    assert_eq!(cfg, reparsed);
}

#[test]
fn sidebar_unknown_position_is_rejected() {
    let input = r#"
[sidebar]
position = "floating"
"#;
    let err = parse_str(input, &path()).expect_err("unknown position rejected");
    assert!(matches!(err, ConfigError::Parse { .. }));
}

#[test]
fn sidebar_unknown_field_is_rejected() {
    let input = r"
[sidebar]
wdith = 20
";
    let err = parse_str(input, &path()).expect_err("typo rejected by deny_unknown_fields");
    assert!(matches!(err, ConfigError::Parse { .. }));
}

// ---------------------------------------------------------------------------
// [status] position  (phux-foz.8)
// ---------------------------------------------------------------------------

#[test]
fn status_position_defaults_to_bottom_when_absent() {
    let cfg = parse_str("", &path()).expect("empty parses");
    assert_eq!(cfg.status.position, StatusPosition::Bottom);
}

#[test]
fn status_position_top_parses_and_round_trips() {
    let input = r#"
[status]
left     = ["session-name"]
position = "top"
"#;
    let cfg = parse_str(input, &path()).expect("[status] position parses");
    assert_eq!(cfg.status.position, StatusPosition::Top);

    let reser = toml::to_string(&cfg).expect("reserialize");
    let reparsed = parse_str(&reser, &path()).expect("reparse");
    assert_eq!(cfg, reparsed);
}

#[test]
fn status_unknown_position_is_rejected() {
    let input = r#"
[status]
position = "floating"
"#;
    let err = parse_str(input, &path()).expect_err("unknown position rejected");
    assert!(matches!(err, ConfigError::Parse { .. }));
}

// ---------------------------------------------------------------------------
// [defaults] sane-default spawn knobs  (phux-4li.1)
// ---------------------------------------------------------------------------

#[test]
fn defaults_spawn_knobs_default_when_absent() {
    // Empty config: all three new knobs default to their shipped values.
    let cfg = parse_str("", &path()).expect("empty parses");
    assert_eq!(cfg.defaults.cwd_inheritance, CwdInheritance::InheritFocused);
    assert_eq!(cfg.defaults.spawn_on_attach, None);
    assert_eq!(cfg.defaults.session_name_template, "default");
    assert_eq!(cfg.defaults.window_size, WindowSize::Smallest);
}

#[test]
fn defaults_term_defaults_to_xterm_256color() {
    // phux-ign: the `defaults.term` baseline is the safe xterm value when
    // the key is absent. A regression here would silently change the TERM
    // advertised to every server-spawned pane.
    let cfg = parse_str("", &path()).expect("empty parses");
    assert_eq!(cfg.defaults.term, "xterm-256color");
}

#[test]
fn defaults_term_round_trips_user_value() {
    // phux-ign: a user can opt into ghostty's extended terminfo by setting
    // `defaults.term`. The value must survive parse + re-serialize.
    let input = r#"
[defaults]
term = "ghostty"
"#;
    let cfg = parse_str(input, &path()).expect("term parses");
    assert_eq!(cfg.defaults.term, "ghostty");

    let reser = toml::to_string(&cfg).expect("reserialize");
    let reparsed = parse_str(&reser, &path()).expect("reparse");
    assert_eq!(cfg, reparsed);
}

#[test]
fn defaults_spawn_knobs_round_trip_user_values() {
    let input = r#"
[defaults]
cwd-inheritance       = "home"
spawn-on-attach       = "/usr/bin/tmux-like"
session-name-template = "phux-${cwd-basename}"
"#;
    let cfg = parse_str(input, &path()).expect("knobs parse");
    assert_eq!(cfg.defaults.cwd_inheritance, CwdInheritance::Home);
    assert_eq!(
        cfg.defaults.spawn_on_attach.as_deref(),
        Some("/usr/bin/tmux-like")
    );
    assert_eq!(cfg.defaults.session_name_template, "phux-${cwd-basename}");

    // Re-serialize and re-parse: PartialEq holds.
    let reser = toml::to_string(&cfg).expect("reserialize");
    let reparsed = parse_str(&reser, &path()).expect("reparse");
    assert_eq!(cfg, reparsed);
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
fn cwd_inheritance_unknown_variant_is_rejected() {
    let input = r#"
[defaults]
cwd-inheritance = "random-walk"
"#;
    let err = parse_str(input, &path()).expect_err("unknown enum variant rejected");
    assert!(matches!(err, ConfigError::Parse { .. }));
}

// ---------------------------------------------------------------------------
// [defaults] window-size  (ADR-0027)
// ---------------------------------------------------------------------------

#[test]
fn window_size_defaults_to_smallest_when_absent() {
    // ADR-0027: default `smallest` never crops content. A regression here
    // would silently change the geometry policy for mirrored views /
    // multi-client attach.
    let cfg = parse_str("", &path()).expect("empty parses");
    assert_eq!(cfg.defaults.window_size, WindowSize::Smallest);
    assert_eq!(WindowSize::default(), WindowSize::Smallest);
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
fn window_size_unknown_variant_is_rejected() {
    let input = r#"
[defaults]
window-size = "fit-to-content"
"#;
    let err = parse_str(input, &path()).expect_err("unknown enum variant rejected");
    assert!(matches!(err, ConfigError::Parse { .. }));
}

#[test]
fn window_size_round_trips_user_value() {
    let input = r#"
[defaults]
window-size = "largest"
"#;
    let cfg = parse_str(input, &path()).expect("window-size parses");
    assert_eq!(cfg.defaults.window_size, WindowSize::Largest);

    let reser = toml::to_string(&cfg).expect("reserialize");
    let reparsed = parse_str(&reser, &path()).expect("reparse");
    assert_eq!(cfg, reparsed);
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
