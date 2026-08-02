use std::path::Path;

use tempfile::TempDir;

use phux_config::{Config, parse_str, plugin};

mod common;
use common::{manifest, write_manifest};

#[test]
fn checked_in_example_manifests_load() -> Result<(), Box<dyn std::error::Error>> {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/plugins");

    let loaded =
        plugin::load_plugin_manifest(&examples.join("provider-showcase/phux-plugin.toml"))?;
    assert_eq!(loaded.id, "com.phux.demo.provider-showcase");
    assert_eq!(loaded.events[0].id, "idle");
    assert_eq!(loaded.panes[0].id, "board");
    assert_eq!(loaded.links[0].id, "ticket");
    assert_eq!(loaded.workspaces[0].id, "ops-bench");
    assert_eq!(loaded.workspaces[0].panes[0].pane, "board");

    let loaded = plugin::load_plugin_manifest(&examples.join("continuum/phux-plugin.toml"))?;
    assert_eq!(loaded.id, "com.phux.demo.continuum");
    assert_eq!(loaded.actions[0].id, "autosave");
    assert_eq!(loaded.actions[1].id, "restore-latest");
    assert_eq!(loaded.events[0].id, "idle-autosave");
    assert_eq!(loaded.events[1].on, "session.changed");
    assert_eq!(loaded.workspaces[0].id, "continuum");
    assert_eq!(loaded.workspaces[0].actions, ["autosave", "restore-latest"]);
    assert_eq!(
        loaded.workspaces[0].events,
        ["idle-autosave", "session-autosave"]
    );
    Ok(())
}

#[test]
fn config_accepts_plugin_manifest_entries() -> Result<(), Box<dyn std::error::Error>> {
    let input = r#"
[[plugins]]
manifest = "/tmp/phux-plugin.toml"
enabled = true
"#;

    let cfg: Config = parse_str(input, Path::new("config.toml"))?;

    assert_eq!(cfg.plugins.len(), 1);
    assert_eq!(cfg.plugins[0].manifest, Path::new("/tmp/phux-plugin.toml"));
    assert!(cfg.plugins[0].enabled);
    Ok(())
}

#[test]
fn plugin_manifest_loads_actions_events_and_panes() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let manifest = write_manifest(
        dir.path(),
        &manifest(
            "example.agent-tools",
            r#"
description = "Agent workflow helpers"
platforms = ["linux", "macos"]

[[actions]]
id = "summarize"
title = "Summarize pane"
contexts = ["pane"]
command = ["python3", "summarize.py"]

[[agents]]
id = "codex"
label = "Codex"
state = "blocked"
attention = "high"
contexts = ["workspace"]

[[events]]
id = "idle"
title = "Pane idle"
on = "pane.idle"
command = ["sh", "-c", "printf idle"]

[[panes]]
id = "board"
title = "Agent Board"
placement = "split"
command = ["agent-board"]

[[links]]
id = "ticket"
title = "Open ticket"
contexts = ["pane"]
schemes = ["https"]
patterns = ["https://linear.app/*"]
command = ["open", "{url}"]

[[workspaces]]
id = "agent-bench"
title = "Agent Bench"
description = "Restore and supervise the agent bench"
contexts = ["workspace"]
agents = ["codex"]
actions = ["summarize"]
events = ["idle"]

[[workspaces.panes]]
id = "board-role"
pane = "board"
role = "board"
"#,
        ),
    );

    let loaded = plugin::load_plugin_manifest(&manifest)?;

    assert_eq!(loaded.id, "example.agent-tools");
    assert_eq!(loaded.plugin_root, dir.path().canonicalize()?);
    assert_eq!(loaded.actions[0].id, "summarize");
    assert_eq!(loaded.agents[0].id, "codex");
    assert_eq!(loaded.agents[0].state, plugin::PluginAgentState::Blocked);
    assert_eq!(
        loaded.agents[0].attention,
        plugin::PluginAgentAttention::High
    );
    assert_eq!(loaded.events[0].id, "idle");
    assert_eq!(loaded.events[0].on, "pane.idle");
    assert_eq!(
        loaded.panes[0].placement,
        plugin::PluginPanePlacement::Split
    );
    assert_eq!(loaded.links[0].id, "ticket");
    assert_eq!(loaded.links[0].schemes, ["https"]);
    assert_eq!(loaded.workspaces[0].id, "agent-bench");
    assert_eq!(loaded.workspaces[0].title, "Agent Bench");
    assert_eq!(loaded.workspaces[0].agents, ["codex"]);
    assert_eq!(loaded.workspaces[0].actions, ["summarize"]);
    assert_eq!(loaded.workspaces[0].events, ["idle"]);
    assert_eq!(loaded.workspaces[0].panes[0].id, "board-role");
    assert_eq!(loaded.workspaces[0].panes[0].pane, "board");
    assert_eq!(loaded.workspaces[0].panes[0].role, "board");
    Ok(())
}

/// Every schema-validation rejection, table-driven: each body must be
/// refused at load time with an error containing the given message.
#[test]
#[allow(clippy::too_many_lines, reason = "table of manifest bodies")]
fn plugin_manifest_validation_rejections() -> Result<(), Box<dyn std::error::Error>> {
    let cases: &[(&str, &str, &str)] = &[
        (
            "duplicate agent ids",
            r#"
[[agents]]
id = "codex"
label = "Codex"

[[agents]]
id = "codex"
label = "Codex again"
"#,
            "duplicate plugin agent id",
        ),
        (
            "duplicate action ids",
            r#"
[[actions]]
id = "run"
title = "Run"
command = ["true"]

[[actions]]
id = "run"
title = "Run again"
command = ["true"]
"#,
            "duplicate plugin action id",
        ),
        (
            "duplicate event provider ids",
            r#"
[[events]]
id = "idle"
title = "Idle"
on = "pane.idle"
command = ["true"]

[[events]]
id = "idle"
title = "Idle again"
on = "pane.idle"
command = ["true"]
"#,
            "duplicate plugin event id",
        ),
        (
            "duplicate widget ids",
            r#"
[[widgets]]
id = "w"
kind = "exec"
command = "a"

[[widgets]]
id = "w"
kind = "exec"
command = "b"
"#,
            "duplicate plugin widget id",
        ),
        (
            "malformed link provider id",
            r#"
[[links]]
id = "bad link"
title = "Bad"
schemes = ["https"]
command = ["true"]
"#,
            "invalid plugin link handler id",
        ),
        (
            "link provider without matchers",
            r#"
[[links]]
id = "ticket"
title = "Ticket"
command = ["true"]
"#,
            "requires at least one scheme or pattern",
        ),
        (
            "workspace referencing an undeclared pane",
            r#"
[[workspaces]]
id = "bench"
title = "Bench"

[[workspaces.panes]]
id = "missing"
pane = "not-declared"
role = "lead"
"#,
            "workspace bench references unknown pane 'not-declared'",
        ),
    ];

    for (what, extra, want) in cases {
        let dir = TempDir::new()?;
        let path = write_manifest(dir.path(), &manifest("example.reject", extra));
        let Err(err) = plugin::load_plugin_manifest(&path) else {
            return Err(format!("{what}: manifest loaded successfully").into());
        };
        assert!(
            err.to_string().contains(want),
            "{what}: error should contain {want:?}; got {err}"
        );
    }
    Ok(())
}

#[test]
fn plugin_manifest_defaults_agent_state_to_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let manifest = write_manifest(
        dir.path(),
        &manifest(
            "example.agent-state",
            r#"
[[agents]]
id = "background-worker"
label = "Background Worker"
"#,
        ),
    );

    let loaded = plugin::load_plugin_manifest(&manifest)?;

    assert_eq!(loaded.agents[0].state, plugin::PluginAgentState::Unknown);
    assert_eq!(
        loaded.agents[0].attention,
        plugin::PluginAgentAttention::Normal
    );
    Ok(())
}

#[test]
fn plugin_manifest_rejects_oversized_files() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let manifest = dir.path().join("phux-plugin.toml");
    std::fs::write(&manifest, "x".repeat(1024 * 1024 + 1))?;

    let Err(err) = plugin::load_plugin_manifest(&manifest) else {
        return Err("oversized manifest loaded successfully".into());
    };
    assert!(
        err.to_string().contains("exceeds"),
        "error should name size limit; got {err}"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn plugin_manifest_parse_errors_use_supplied_symlink_path() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = TempDir::new()?;
    let target_dir = dir.path().join("private-target");
    std::fs::create_dir_all(&target_dir)?;
    let target = target_dir.join("phux-plugin.toml");
    std::fs::write(&target, "not valid = [")?;
    let link = dir.path().join("public-link.toml");
    std::os::unix::fs::symlink(&target, &link)?;

    let Err(err) = plugin::load_plugin_manifest(&link) else {
        return Err("malformed symlinked manifest loaded successfully".into());
    };
    let message = err.to_string();
    assert!(
        message.contains("public-link.toml"),
        "parse error should report caller-facing symlink path; got {message}"
    );
    assert!(
        !message.contains("private-target"),
        "parse error should not leak canonical target path; got {message}"
    );
    Ok(())
}

#[test]
fn plugin_action_keys_field_parses_and_defaults_to_none() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = TempDir::new()?;
    let manifest = write_manifest(
        dir.path(),
        &manifest(
            "example.keys",
            r#"
[[actions]]
id = "bound"
title = "Bound action"
command = ["true"]
keys = "g"

[[actions]]
id = "unbound"
title = "Unbound action"
command = ["true"]

[[actions]]
id = "blank"
title = "Blank keys action"
command = ["true"]
keys = "   "
"#,
        ),
    );

    let loaded = plugin::load_plugin_manifest(&manifest)?;

    assert_eq!(loaded.actions[0].keys.as_deref(), Some("g"));
    assert_eq!(loaded.actions[1].keys, None, "keys defaults to None");
    assert_eq!(
        loaded.actions[2].keys, None,
        "whitespace-only keys normalizes to None"
    );
    Ok(())
}

#[test]
fn load_enabled_manifests_skips_disabled_and_broken_plugins()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let config_path = dir.path().join("config.toml");
    // Three manifests: one healthy + enabled, one healthy + disabled, one
    // missing entirely. Only the first must load; the rest are skipped
    // without failing the batch.
    common::write(
        dir.path(),
        "good.toml",
        &manifest(
            "example.good",
            r#"
[[actions]]
id = "act"
title = "Act"
command = ["true"]
"#,
        ),
    );
    let off = common::write(dir.path(), "off.toml", &manifest("example.off", ""));

    let entries = vec![
        plugin::PluginConfigEntry {
            // Relative path: resolves against the config file's directory.
            manifest: std::path::PathBuf::from("good.toml"),
            enabled: true,
        },
        plugin::PluginConfigEntry {
            manifest: off,
            enabled: false,
        },
        plugin::PluginConfigEntry {
            manifest: dir.path().join("missing.toml"),
            enabled: true,
        },
    ];

    let manifests = plugin::load_enabled_manifests(&config_path, &entries);

    assert_eq!(manifests.len(), 1, "only the enabled, healthy manifest");
    assert_eq!(manifests[0].id, "example.good");
    Ok(())
}

// ---------------------------------------------------------------------------
// phux-r82.6: [[widgets]] status-bar contributions
// ---------------------------------------------------------------------------

#[test]
fn plugin_manifest_loads_widget_contributions() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let manifest = write_manifest(
        dir.path(),
        &manifest(
            "example.battery",
            r#"
[[widgets]]
id = "battery"
slot = "right"
kind = "exec"
command = "battery.sh"
interval = "30s"

[[widgets]]
id = "branch"
kind = "exec"
command = ["git-branch-widget"]
"#,
        ),
    );

    let loaded = plugin::load_plugin_manifest(&manifest)?;

    assert_eq!(loaded.widgets.len(), 2);
    assert_eq!(loaded.widgets[0].id, "battery");
    assert_eq!(loaded.widgets[0].kind, "exec");
    assert_eq!(
        loaded.widgets[0].slot,
        plugin::PluginWidgetSlot::Right,
        "explicit slot"
    );
    assert_eq!(
        loaded.widgets[0].opts.get("interval"),
        Some(&toml::Value::String("30s".to_owned())),
        "kind-specific options ride the flattened opts map"
    );
    assert_eq!(
        loaded.widgets[1].slot,
        plugin::PluginWidgetSlot::Right,
        "slot defaults to right"
    );
    Ok(())
}

#[test]
fn merge_widget_contributions_appends_after_user_widgets_and_drops_invalid()
-> Result<(), Box<dyn std::error::Error>> {
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget};

    let dir = TempDir::new()?;
    let manifest = write_manifest(
        dir.path(),
        &manifest(
            "example.mixed",
            r#"
[[widgets]]
id = "ok"
slot = "left"
kind = "exec"
command = "ok.sh"

[[widgets]]
id = "bad-kind"
kind = "no-such-widget"

[[widgets]]
id = "bad-opts"
kind = "exec"
interval = "30s"
"#,
        ),
    );
    let loaded = plugin::load_plugin_manifest(&manifest)?;

    let mut status = StatusCfg {
        left: vec![Widget::Bare("session-name".to_owned())],
        ..StatusCfg::default()
    };
    plugin::merge_widget_contributions(
        &mut status,
        std::slice::from_ref(&loaded),
        &WidgetRegistry::with_builtins(),
    );

    // The valid contribution appended AFTER the user's widget; the unknown
    // kind and the command-less exec were both dropped.
    assert_eq!(status.left.len(), 2);
    assert!(matches!(&status.left[0], Widget::Bare(k) if k == "session-name"));
    match &status.left[1] {
        Widget::Spec(spec) => assert_eq!(spec.kind, "exec"),
        other @ Widget::Bare(_) => panic!("expected contributed spec, got {other:?}"),
    }
    assert!(status.center.is_empty() && status.right.is_empty());
    Ok(())
}

// ---------------------------------------------------------------------------
// The `min_phux_version` gate (phux-r82.2)
// ---------------------------------------------------------------------------

/// Table-driven version gate: a floor at the current version loads
/// (equality is the boundary case); a future floor is rejected with an
/// error naming the plugin, its floor, and the running version — so both
/// `phux plugin link` and every load-time consumer see the same refusal;
/// a floor that is not a dotted numeric version is a schema error, not a
/// silent pass.
#[test]
fn min_phux_version_gate_accepts_current_and_rejects_future_or_malformed()
-> Result<(), Box<dyn std::error::Error>> {
    let current = plugin::CURRENT_PHUX_VERSION;
    // (case, floor, expected error substrings; empty = must load)
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        ("current version loads", current, vec![]),
        (
            "future floor rejected",
            "99.0.0",
            vec!["example.gate", "99.0.0", current],
        ),
        (
            "malformed floor rejected",
            "latest",
            vec!["malformed min_phux_version"],
        ),
    ];

    for (what, floor, want) in cases {
        let dir = TempDir::new()?;
        let body = format!(
            "id = \"example.gate\"\nname = \"Gate\"\nversion = \"0.1.0\"\nmin_phux_version = \"{floor}\"\n"
        );
        let path = write_manifest(dir.path(), &body);
        let result = plugin::load_plugin_manifest(&path);
        if want.is_empty() {
            let loaded = result.unwrap_or_else(|e| panic!("{what}: must load: {e}"));
            assert_eq!(loaded.id, "example.gate");
            assert_eq!(loaded.min_phux_version, current);
        } else {
            let err = result
                .err()
                .unwrap_or_else(|| panic!("{what}: must be rejected"));
            let message = err.to_string();
            for needle in want {
                assert!(message.contains(needle), "{what}: {message}");
            }
        }
    }
    Ok(())
}

/// The best-effort batch loader skips (never propagates) a plugin gated
/// out by `min_phux_version`, so one too-new plugin cannot take down the
/// TUI or server consuming the healthy ones.
#[test]
fn load_enabled_manifests_skips_version_gated_plugin() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let config_path = dir.path().join("config.toml");
    let good = common::write(
        dir.path(),
        "good.toml",
        "id = \"example.good-floor\"\nname = \"Good\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.1\"\n",
    );
    let future = common::write(
        dir.path(),
        "future.toml",
        "id = \"example.future-floor\"\nname = \"Future\"\nversion = \"0.1.0\"\nmin_phux_version = \"99.0.0\"\n",
    );

    let entries = vec![
        plugin::PluginConfigEntry {
            manifest: good,
            enabled: true,
        },
        plugin::PluginConfigEntry {
            manifest: future,
            enabled: true,
        },
    ];

    let manifests = plugin::load_enabled_manifests(&config_path, &entries);

    assert_eq!(manifests.len(), 1, "the gated plugin is skipped");
    assert_eq!(manifests[0].id, "example.good-floor");
    Ok(())
}
