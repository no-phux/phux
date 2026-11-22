//! Unit tests for the launch-only shims (`codex`, `opencode`, `pi`).

use super::BLOCK_BEGIN;
use super::agents::{
    AGENT_SHIM_SCHEMA, install_agent_into, render_agent_wrapper, uninstall_agent_from,
};
use super::install::{install_claude_into, installed_shim_schema, uninstall_claude_from};
use crate::skill::setup::SkillAgent;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Every rendered wrapper parses as `/bin/sh`.
#[test]
fn every_agent_wrapper_is_valid_sh() {
    for agent in [SkillAgent::Codex, SkillAgent::Opencode, SkillAgent::Pi] {
        let script = render_agent_wrapper(
            agent,
            Path::new("/opt/real"),
            Path::new("/opt/phux"),
            Path::new("/opt/shim"),
        )
        .unwrap();
        let out = std::process::Command::new("sh")
            .args(["-n", "-c", &script])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{agent:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A codex shim beside Claude's: one rc block for both, a one-shot run goes
/// straight to the real binary, and the rc block leaves with the last shim.
#[test]
fn codex_shim_shares_the_rc_block_and_passes_one_shot_runs_through() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let shim_dir = dir.path().join("shims");
    let rc = dir.path().join("rc");
    let phux = dir.path().join("phux");
    let real_claude = dir.path().join("real-claude");
    let real_codex = dir.path().join("real-codex");
    executable(&real_claude, "#!/bin/sh\nexit 0\n");
    executable(&real_codex, "#!/bin/sh\necho \"real:$*\"\n");
    let user_rc = "# my rc\n";
    std::fs::write(&rc, user_rc).unwrap();
    // Steer resolution through the manifest, as a reinstall would.
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(
        shim_dir.join("codex-install.json"),
        serde_json::json!({ "real": real_codex, "rc": rc }).to_string(),
    )
    .unwrap();

    install_claude_into(&shim_dir, &rc, "bash", Some(&real_claude), &phux).unwrap();
    let report = install_agent_into(SkillAgent::Codex, &shim_dir, &rc, "bash", &phux).unwrap();
    let shim = shim_dir.join("codex");
    assert_eq!(report.shim, shim);
    assert_eq!(installed_shim_schema(&shim), Some(AGENT_SHIM_SCHEMA));
    let script = std::fs::read_to_string(&shim).unwrap();
    assert!(script.contains("--name codex --kind codex"), "{script}");
    assert_eq!(
        std::fs::read_to_string(&rc)
            .unwrap()
            .matches(BLOCK_BEGIN)
            .count(),
        1
    );

    // Not a tty and a one-shot subcommand: exec the real codex, never phux.
    let out = std::process::Command::new(&shim)
        .args(["exec", "hello"])
        .env_remove("PHUX_TERMINAL_ID")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "real:exec hello\n");

    // Claude's shim is still there, so the rc block stays.
    assert!(
        uninstall_agent_from(SkillAgent::Codex, &shim_dir)
            .unwrap()
            .is_some()
    );
    assert!(std::fs::read_to_string(&rc).unwrap().contains(BLOCK_BEGIN));
    assert!(!shim.exists());
    // The last shim takes the block with it.
    uninstall_claude_from(&shim_dir).unwrap();
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), user_rc);
    assert_eq!(std::fs::read_dir(&shim_dir).unwrap().count(), 0);
    assert!(
        uninstall_agent_from(SkillAgent::Codex, &shim_dir)
            .unwrap()
            .is_none()
    );
}
