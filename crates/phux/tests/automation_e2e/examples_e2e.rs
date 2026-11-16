//! The published introductions must work against the same CLI CI built.

use crate::common;

/// Run the repository script `script` under `program` against the CLI CI
/// built, and require it to pass; `what` names it in the failure.
fn assert_script_passes(program: &str, script: &str, env: &[(&str, &str)], what: &str) {
    let output = std::process::Command::new(program)
        .arg(crate::runner::manifest_dir().join("../..").join(script))
        .env("PHUX", crate::runner::phux_bin())
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run the repository script");
    assert!(
        output.status.success(),
        "{what} failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "spawns private servers and PTYs; run through just e2e"]
fn agent_verify_self_test_covers_the_harness_without_real_agents() {
    assert_script_passes(
        "python3",
        "scripts/agent-verify/self_test.py",
        &[("PYTHONDONTWRITEBYTECODE", "1")],
        "agent-verify self-test",
    );
}

#[test]
#[ignore = "spawns private servers and PTYs; run through just e2e"]
fn introductory_examples_use_real_output_and_propagate_failures() {
    assert_script_passes("bash", "scripts/examples-smoke.sh", &[], "examples");
}

#[test]
#[ignore = "spawns a private server and PTY; run through just e2e"]
fn scripting_doc_automation_drives_only_the_shell_it_creates() {
    let server = common::ServerGuard::start("readme");
    let doc =
        std::fs::read_to_string(crate::runner::manifest_dir().join("../../docs/scripting.md"))
            .expect("read docs/scripting.md");
    let script = doc
        .split("```sh\n")
        .skip(1)
        .filter_map(|block| block.split_once("```"))
        .map(|(script, _)| script)
        .find(|script| script.contains("session=\"phux-demo-$$\""))
        .expect("docs/scripting.md automation example");
    let directory = tempfile::tempdir().expect("private command path");
    std::os::unix::fs::symlink(crate::runner::phux_bin(), directory.path().join("phux"))
        .expect("same-checkout CLI on PATH");
    let mut command = std::process::Command::new("sh");
    command
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                directory.path().display(),
                std::env::var("PATH").expect("PATH")
            ),
        )
        .env("PHUX_SOCKET", &server.socket)
        .env("PHUX_NO_UPDATE_CHECK", "1")
        .args(["-eu", "-c", script]);
    server.xdg.apply(&mut command);
    let output = command
        .output()
        .expect("execute docs/scripting.md automation");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello from phux"), "{stdout}");
    assert!(stdout.contains("The shell is still running."), "{stdout}");
    let inventory = server.success(&["ls", "--json"]);
    assert!(inventory.contains("phux-demo-"), "{inventory}");
    let original = server.success(&["snapshot", "--json", "work"]);
    assert!(
        !original.contains("hello from phux"),
        "the example typed into the pre-existing pane: {original}"
    );
}

#[test]
#[ignore = "spawns private servers and imports an archive; run through just e2e"]
fn relocated_continuum_restores_an_archive_from_retained_state() {
    let source = common::ServerGuard::start("old-plugin-state");
    source.success(&["rename", "work", "retained-project"]);
    let directory = tempfile::tempdir().expect("migration fixture");
    let old = directory.path().join("old-state");
    let new = directory.path().join("new-state");
    std::fs::create_dir(&old).expect("old plugin state");
    source.success(&[
        "archive",
        "export",
        "--output",
        old.join("default.json").to_str().expect("path"),
    ]);
    let status = std::process::Command::new("python3")
        .args([
            "-c",
            "import shutil,sys; shutil.copytree(sys.argv[1], sys.argv[2], symlinks=True)",
        ])
        .arg(&old)
        .arg(&new)
        .status()
        .expect("copy retained plugin state");
    assert!(status.success());
    let target = common::ServerGuard::builder("new-plugin-state")
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .env("XDG_STATE_HOME", directory.path().join("state"))
        .start();
    let plugin = crate::runner::manifest_dir().join("../../plugins/continuum");
    let mut link = common::phux_cmd(crate::runner::phux_bin());
    target.xdg.apply(&mut link);
    let output = link
        .args(["plugin", "link", plugin.to_str().expect("plugin path")])
        .output()
        .expect("link relocated plugin");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut restore = common::phux_cmd(crate::runner::phux_bin());
    target.xdg.apply(&mut restore);
    let output = restore
        .args([
            "config",
            "run",
            "com.phux.demo.continuum",
            "restore-latest",
            "--json",
        ])
        .env("PHUX_BIN", crate::runner::phux_bin())
        .env("PHUX_SOCKET", &target.socket)
        .env("PHUX_CONTINUUM_DIR", &new)
        .output()
        .expect("restore retained archive through relocated plugin");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inventory = target.success(&["ls", "--json"]);
    assert!(inventory.contains("retained-project"), "{inventory}");
    assert!(
        old.join("default.json").is_file(),
        "migration must not delete the old archive"
    );
}
