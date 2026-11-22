#![allow(clippy::unwrap_used, reason = "test fixture assertions")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{AgentReport, Ctx, HOOK_COMMAND, Mode, STUB, SkillAgent, State, apply};

/// A temp HOME where only the given agents' config directories exist and
/// nothing is on `PATH`.
fn home(agents: &[&str]) -> (tempfile::TempDir, Ctx) {
    let dir = tempfile::tempdir().unwrap();
    for config in agents {
        fs::create_dir_all(dir.path().join(config)).unwrap();
    }
    let ctx = Ctx {
        home: dir.path().to_path_buf(),
        base: dir.path().to_path_buf(),
        path: None,
        project: false,
        shim: Some(dir.path().join(".local/share/phux/shims/claude")),
    };
    (dir, ctx)
}

fn setup(ctx: &Ctx) -> Vec<AgentReport> {
    apply(ctx, Mode::Setup { force: false }, &[], false)
        .unwrap()
        .agents
}

fn agent<'a>(report: &'a [AgentReport], name: &str) -> &'a AgentReport {
    report.iter().find(|agent| agent.agent == name).unwrap()
}

fn settings(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join(".claude/settings.json")).unwrap()).unwrap()
}

#[test]
fn a_fresh_install_writes_each_directory_once_and_the_hook() {
    let (dir, ctx) = home(&[".claude", ".codex", ".pi"]);
    let report = setup(&ctx);

    let claude = agent(&report, "claude");
    assert!(claude.detected);
    assert_eq!(claude.skill.state, State::Installed);
    assert_eq!(claude.hook.state, State::Installed);
    let stub = fs::read_to_string(dir.path().join(".claude/skills/phux/SKILL.md")).unwrap();
    assert_eq!(stub, STUB);
    assert!(stub.starts_with("---\nname: phux\n"));
    assert!(stub.contains("phux-stub: \"1\""));

    // Codex and pi share one directory and one result; OpenCode was not found.
    for name in ["codex", "pi"] {
        assert_eq!(agent(&report, name).skill.state, State::Installed);
    }
    assert_eq!(agent(&report, "opencode").skill.state, State::Skipped);
    assert!(dir.path().join(".agents/skills/phux/SKILL.md").exists());

    let hooks = &settings(dir.path())["hooks"]["SessionStart"];
    assert_eq!(hooks[0]["hooks"][0]["command"], HOOK_COMMAND);
    assert_eq!(hooks[0]["hooks"][0]["timeout"], 5);
}

#[test]
fn no_agent_found_writes_only_the_shared_directory() {
    let (dir, ctx) = home(&[]);
    let report = setup(&ctx);
    assert_eq!(agent(&report, "claude").skill.state, State::Skipped);
    assert_eq!(agent(&report, "codex").skill.state, State::Installed);
    assert!(dir.path().join(".agents/skills/phux/SKILL.md").exists());
    assert!(!dir.path().join(".claude").exists());
}

#[test]
fn setup_is_idempotent_and_refreshes_a_stale_stub() {
    let (dir, ctx) = home(&[".claude"]);
    setup(&ctx);
    let settings_before = fs::read(dir.path().join(".claude/settings.json")).unwrap();

    let again = setup(&ctx);
    assert_eq!(agent(&again, "claude").skill.state, State::UpToDate);
    assert_eq!(agent(&again, "claude").hook.state, State::UpToDate);
    assert_eq!(
        fs::read(dir.path().join(".claude/settings.json")).unwrap(),
        settings_before
    );

    let stub = dir.path().join(".claude/skills/phux/SKILL.md");
    fs::write(
        &stub,
        "---\nname: phux\nmetadata:\n  phux-stub: \"0\"\n---\nold\n",
    )
    .unwrap();
    assert_eq!(
        apply(&ctx, Mode::Status, &[], false).unwrap().agents[0]
            .skill
            .state,
        State::Stale
    );
    assert_eq!(agent(&setup(&ctx), "claude").skill.state, State::Updated);
    assert_eq!(fs::read_to_string(&stub).unwrap(), STUB);
}

#[test]
fn a_foreign_skill_is_a_conflict_until_forced() {
    let (dir, ctx) = home(&[".claude"]);
    let stub = dir.path().join(".claude/skills/phux/SKILL.md");
    fs::create_dir_all(stub.parent().unwrap()).unwrap();
    fs::write(&stub, "---\nname: phux\n---\nmine\n").unwrap();

    assert_eq!(agent(&setup(&ctx), "claude").skill.state, State::Conflict);
    assert_eq!(
        fs::read_to_string(&stub).unwrap(),
        "---\nname: phux\n---\nmine\n"
    );

    // Remove never touches it either.
    let removed = apply(&ctx, Mode::Remove, &[], false).unwrap().agents;
    assert_eq!(agent(&removed, "claude").skill.state, State::Conflict);
    assert!(stub.exists());

    let forced = apply(&ctx, Mode::Setup { force: true }, &[], false)
        .unwrap()
        .agents;
    assert_eq!(agent(&forced, "claude").skill.state, State::Installed);
    assert_eq!(fs::read_to_string(&stub).unwrap(), STUB);
}

#[test]
fn the_published_using_phux_skill_counts_as_present() {
    let (dir, ctx) = home(&[".codex"]);
    let using = dir.path().join(".agents/skills/using-phux/SKILL.md");
    fs::create_dir_all(using.parent().unwrap()).unwrap();
    fs::write(&using, "---\nname: using-phux\n---\n").unwrap();

    assert_eq!(agent(&setup(&ctx), "codex").skill.state, State::Present);
    assert!(!dir.path().join(".agents/skills/phux").exists());
}

#[test]
fn remove_takes_out_only_what_setup_installed() {
    let (dir, ctx) = home(&[".claude", ".codex"]);
    let path = dir.path().join(".claude/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"model":"opus","hooks":{"SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
    )
    .unwrap();
    setup(&ctx);

    let report = apply(&ctx, Mode::Remove, &[], false).unwrap().agents;
    assert_eq!(agent(&report, "claude").skill.state, State::Removed);
    assert_eq!(agent(&report, "claude").hook.state, State::Removed);
    assert_eq!(agent(&report, "codex").skill.state, State::Removed);
    assert!(!dir.path().join(".claude/skills/phux").exists());
    assert!(!dir.path().join(".agents/skills/phux").exists());
    assert_eq!(
        settings(dir.path()),
        json!({"model":"opus","hooks":{"SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"echo hi"}]}]}})
    );

    let again = apply(&ctx, Mode::Remove, &[], false).unwrap().agents;
    assert_eq!(agent(&again, "claude").skill.state, State::Missing);
    assert_eq!(agent(&again, "claude").hook.state, State::Missing);
}

#[test]
fn the_hook_merge_keeps_unrelated_settings_in_order() {
    let (dir, ctx) = home(&[".claude"]);
    let path = dir.path().join(".claude/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"theme":"dark","permissions":{"allow":["Bash(ls)"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]},"zeta":1}"#,
    )
    .unwrap();
    setup(&ctx);

    let merged = settings(dir.path());
    assert_eq!(merged["theme"], "dark");
    assert_eq!(merged["permissions"]["allow"][0], "Bash(ls)");
    assert_eq!(
        merged["hooks"]["Stop"][0]["hooks"][0]["command"],
        "say done"
    );
    assert_eq!(merged["zeta"], 1);
    let keys: Vec<_> = merged.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["theme", "permissions", "hooks", "zeta"]);
    assert_eq!(
        merged["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        HOOK_COMMAND
    );
}

#[test]
fn unreadable_settings_are_never_rewritten() {
    let (dir, ctx) = home(&[".claude"]);
    let path = dir.path().join(".claude/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{ not json").unwrap();
    let report = setup(&ctx);
    let hook = &agent(&report, "claude").hook;
    assert_eq!(hook.state, State::Conflict);
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
}

#[test]
fn the_hook_is_skipped_when_the_shim_or_plugin_owns_phux_hooks() {
    let (dir, ctx) = home(&[".claude"]);
    let shim = ctx.shim.clone().unwrap();
    fs::create_dir_all(shim.parent().unwrap()).unwrap();
    fs::write(&shim, "#!/bin/sh\n").unwrap();
    let report = setup(&ctx);
    let hook = &agent(&report, "claude").hook;
    assert_eq!(hook.state, State::Skipped);
    assert!(hook.reason.as_deref().unwrap().contains("install-claude"));
    assert!(!dir.path().join(".claude/settings.json").exists());
    fs::remove_file(&shim).unwrap();

    let path = dir.path().join(".claude/settings.json");
    fs::write(&path, r#"{"enabledPlugins":{"phux@phux":true}}"#).unwrap();
    let report = setup(&ctx);
    let hook = &agent(&report, "claude").hook;
    assert_eq!(hook.state, State::Skipped);
    assert!(hook.reason.as_deref().unwrap().contains("plugin"));
    assert_eq!(
        settings(dir.path()),
        json!({"enabledPlugins":{"phux@phux":true}})
    );
}

#[test]
fn dry_run_and_explicit_agents_change_nothing_else() {
    let (dir, ctx) = home(&[".claude", ".codex"]);
    let report = apply(&ctx, Mode::Setup { force: false }, &[], true)
        .unwrap()
        .agents;
    assert_eq!(agent(&report, "claude").skill.state, State::Installed);
    assert_eq!(agent(&report, "claude").hook.state, State::Installed);
    assert!(!dir.path().join(".claude/skills").exists());
    assert!(!dir.path().join(".claude/settings.json").exists());

    let report = apply(
        &ctx,
        Mode::Setup { force: false },
        &[SkillAgent::Codex],
        false,
    )
    .unwrap()
    .agents;
    assert_eq!(agent(&report, "claude").skill.state, State::Skipped);
    assert_eq!(agent(&report, "codex").skill.state, State::Installed);
    assert!(!dir.path().join(".claude/skills").exists());
}

#[test]
fn the_json_shape_is_stable() {
    let (_dir, ctx) = home(&[".claude"]);
    let report = serde_json::to_value(apply(&ctx, Mode::Status, &[], false).unwrap()).unwrap();
    assert_eq!(report["schema"], "phux.skill-setup/v1");
    assert_eq!(report["action"], "status");
    assert_eq!(report["scope"], "user");
    let claude = &report["agents"][0];
    assert_eq!(claude["agent"], "claude");
    assert_eq!(claude["detected"], true);
    assert_eq!(claude["skill"]["state"], "missing");
    assert_eq!(claude["hook"]["state"], "missing");
    assert!(claude["hook"].get("reason").is_none());
    assert_eq!(report["agents"][1]["hook"]["state"], "skipped");
}
