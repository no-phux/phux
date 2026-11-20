#![allow(clippy::expect_used, reason = "tests")]

use crate::common::Out;
use crate::support::{PLUGIN_ID, demo_xdg, repo_root};

const INSPECT_ACTION: &str = "inspect";
const LIST_ACTION: &str = "list-integrations";
const VALIDATE_ACTION: &str = "validate-integrations";
const DETECT_ACTION: &str = "detect-agents";
const SMOKE_AGENT_WRAP_ACTION: &str = "smoke-agent-wrap";

fn run_demo(args: &[&str]) -> (i32, String, String) {
    run_demo_with_env(args, &[])
}

fn run_demo_with_env(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    Out::of(
        crate::common::phux_cmd(crate::runner::phux_bin())
            .env("XDG_CONFIG_HOME", demo_xdg())
            .env("PHUX_BIN", crate::runner::phux_bin())
            .envs(envs.iter().copied())
            .args(args),
    )
    .into_parts()
}

fn stdout_from_json(output: &serde_json::Value) -> &str {
    output["stdout"].as_str().expect("stdout field")
}

#[test]
fn inspect_action_reports_runtime_context() {
    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, INSPECT_ACTION]);
    assert_eq!(code, 0, "config run should succeed; stderr={stderr}");
    assert!(stdout.contains(&format!("plugin_id={PLUGIN_ID}")));
    assert!(stdout.contains(&format!("action_id={INSPECT_ACTION}")));

    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, INSPECT_ACTION, "--json"]);
    assert_eq!(code, 0, "config run --json should succeed; stderr={stderr}");
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("run stdout is JSON");
    assert_eq!(output["plugin_id"], PLUGIN_ID);
    assert_eq!(output["action_id"], INSPECT_ACTION);
    assert_eq!(output["outcome"], "completed");
    assert_eq!(output["exit_code"], 0);
    assert!(stdout_from_json(&output).contains(&format!("plugin_id={PLUGIN_ID}")));
}

#[test]
fn inspect_action_receives_headless_target_and_input_without_a_client() {
    let (code, stdout, stderr) = run_demo(&[
        "config",
        "run",
        PLUGIN_ID,
        INSPECT_ACTION,
        "--target-json",
        r#"{"host":"hub","satellite":"worker","resource_handle":"worker/@3"}"#,
        "--input-json",
        r#"{"message":"literal ; $HOME"}"#,
    ]);
    assert_eq!(code, 0, "{stderr}");
    let context = stdout
        .lines()
        .find_map(|line| line.strip_prefix("context="))
        .expect("context");
    let context: serde_json::Value = serde_json::from_str(context).expect("JSON context");
    assert_eq!(context["source"], "cli");
    assert_eq!(context["observed"], false);
    assert_eq!(context["target"]["resource_handle"], "worker/@3");
    assert_eq!(context["input"]["message"], "literal ; $HOME");
    assert!(context.get("client").is_none());
}

#[test]
fn integration_template_actions_are_local_and_validated() {
    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, LIST_ACTION, "--json"]);
    assert_eq!(code, 0, "list integrations should succeed; stderr={stderr}");
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("list stdout is JSON");
    let listed = stdout_from_json(&output);
    assert!(listed.contains("codex\tCodex\tterminal-agent\t0.2.0\topt-in\tcodex"));
    assert!(listed.contains("claude-code\tClaude Code\tterminal-agent\t0.2.0\topt-in\tclaude"));
    assert!(listed.contains("grok\tGrok Build\tterminal-agent\t0.1.0\topt-in\tgrok"));
    assert!(listed.contains("generic-shell-agent\tGeneric Shell Agent"));
    assert!(listed.contains("pi\tPi\tterminal-agent\t0.1.0\topt-in\tpi"));
    assert!(listed.contains("omp\tOh My Pi\tterminal-agent\t0.1.0\topt-in\tomp"));
    assert!(listed.contains("opencode\tOpenCode\tterminal-agent\t0.1.0\topt-in\topencode"));
    assert!(listed.contains("amp\tAmp\tterminal-agent\t0.1.0\topt-in\tamp"));
    assert!(listed.contains("cursor-agent\tCursor Agent\tterminal-agent\t0.1.0\topt-in\tagent"));
    assert!(listed.contains("goose\tGoose\tterminal-agent\t0.1.0\topt-in\tgoose"));

    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, VALIDATE_ACTION, "--json"]);
    assert_eq!(
        code, 0,
        "validate integrations should succeed; stderr={stderr}"
    );
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("validate stdout is JSON");
    assert!(stdout_from_json(&output).contains("validated 11 integration templates"));

    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, DETECT_ACTION, "--json"]);
    assert_eq!(
        code, 0,
        "explicit action invocation opts in; stderr={stderr}"
    );
    let output: serde_json::Value =
        serde_json::from_str(&stdout).expect("detection stdout is JSON");
    assert!(stdout_from_json(&output).starts_with("id\tdisplay_name\tcommand\tstate\n"));
}

#[test]
fn agent_detection_is_opt_in_and_path_overridable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let codex = tmp.path().join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").expect("fake codex");
    let mut perms = std::fs::metadata(&codex)
        .expect("fake codex metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&codex, perms).expect("fake codex executable");

    let fake_path = tmp.path().to_str().expect("utf8 tempdir");
    let (code, stdout, stderr) = Out::of(
        std::process::Command::new("/bin/sh")
            .arg(repo_root().join("plugins/agent-tools/scripts/detect-agents.sh"))
            .env("PHUX_AGENT_TOOLS_DETECT", "1")
            .env("PHUX_AGENT_TOOLS_PATH", fake_path),
    )
    .into_parts();
    assert_eq!(code, 0, "opt-in detection should succeed; stderr={stderr}");
    assert!(stdout.contains("codex\tCodex\tcodex\tavailable"));
    assert!(stdout.contains("claude-code\tClaude Code\tclaude\tmissing"));
}

/// The wrapper smoke covers identity cleanup plus complete one-turn stream
/// records, exact child targeting, provider failure, command lookup failure,
/// and signal forwarding/reaping against a stub `phux`, with no server.
#[test]
fn agent_wrap_writes_and_clears_the_identity_record() {
    let (code, stdout, stderr) = run_demo(&[
        "config",
        "run",
        PLUGIN_ID,
        SMOKE_AGENT_WRAP_ACTION,
        "--json",
    ]);
    assert_eq!(
        code, 0,
        "agent wrap smoke should succeed; stderr={stderr}\nstdout={stdout}"
    );
    let output: serde_json::Value =
        serde_json::from_str(&stdout).expect("agent wrap smoke stdout is JSON");
    assert_eq!(output["action_id"], SMOKE_AGENT_WRAP_ACTION);
    assert_eq!(output["exit_code"], 0);
    assert!(
        stdout_from_json(&output).contains("agent wrap smoke ok"),
        "smoke script should confirm the set/clear record path: {output}"
    );
}

#[test]
fn action_does_not_inherit_an_ambient_socket() {
    let (code, stdout, stderr) = run_demo_with_env(
        &["config", "run", PLUGIN_ID, INSPECT_ACTION, "--json"],
        &[("PHUX_SOCKET", "/tmp/not-for-this-plugin.socket")],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.contains("not-for-this-plugin.socket"));
}

/// ADR-0151: the template claiming each in-shell integration's `AgentSession`
/// provider declares the source-verified resume (and, only where the provider
/// accepts one, fresh-identity) arguments `archive import` rebuilds.
#[test]
fn shipped_templates_resume_the_providers_in_shell_integrations_report() {
    let dir = repo_root().join("plugins/agent-tools/integrations");
    let base = vec!["agent".to_owned()];
    for (file, provider, resume, fresh) in [
        (
            "claude-code.toml",
            "claude",
            "--resume",
            Some("--session-id"),
        ),
        ("pi.toml", "pi", "--session", Some("--session-id")),
        ("omp.toml", "omp", "--resume", None),
        ("opencode.toml", "opencode", "--session", None),
    ] {
        let template = phux_config::integration::load_integration_template(&dir.join(file))
            .unwrap_or_else(|err| panic!("{file} must load: {err}"));
        assert_eq!(
            template
                .agent_identity
                .as_ref()
                .and_then(|identity| identity.kind.as_deref()),
            Some(provider),
            "{file} must claim provider {provider}"
        );
        let policy = template
            .session_identity
            .unwrap_or_else(|| panic!("{file} declares session identity"));
        assert_eq!(
            policy.resume_argv(&base, "abc").expect("resumable"),
            ["agent", resume, "abc"],
            "{file} resume argv"
        );
        assert_eq!(
            policy.fresh_argv(&base, "abc").ok(),
            fresh.map(|flag| vec!["agent".to_owned(), flag.to_owned(), "abc".to_owned()]),
            "{file} fresh argv"
        );
    }
}
