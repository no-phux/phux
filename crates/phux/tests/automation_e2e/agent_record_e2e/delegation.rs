/// ADR-0075 point 5 on `agent start`: it types the launch line over
/// acknowledged input, so a `%name` whose record has the withdrawn shape (a
/// `kind`, `state: unknown`) is refused as `agent_withdrawn` before anything
/// is typed, as `send-keys`, `paste`, and `agent prompt` refuse it.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn agent_start_refuses_a_withdrawn_name() {
    let server = ServerGuard::start("agent");
    // `set` without `--state` leaves `unknown`: with a kind, the withdrawn shape.
    server.agent(&["set", SESSION, "--name", "build", "--kind", "codex"]);
    // The repo's example plugin config supplies a `codex` integration, so the
    // launch plan resolves the same on every host and the guard is what refuses.
    let config = crate::runner::manifest_dir()
        .join("../../plugins/agent-tools/config")
        .canonicalize()
        .expect("example plugin config");
    let out = common::phux_cmd(crate::runner::phux_bin())
        .env("XDG_CONFIG_HOME", &config)
        .args([
            "agent", "start", "--json", "--kind", "codex", "--target", "%build",
        ])
        .arg("--socket")
        .arg(&server.socket)
        .arg("reviewer")
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent start");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr={stderr}");
    let error: serde_json::Value = stderr
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("no JSON error document: {stderr}"));
    assert_eq!(error["error"]["code"], "agent_withdrawn", "{stderr}");
}

/// A plugin config with one integration, `deleg-agent`, whose launch runs
/// `script` from the plugin root. Returns the `XDG_CONFIG_HOME` to use.
fn write_script_integration(root: &std::path::Path, script: &str) -> PathBuf {
    let plugin = root.join("plugin");
    let scripts = plugin.join("scripts");
    let integrations = plugin.join("integrations");
    let xdg = root.join("xdg");
    for dir in [&scripts, &integrations, &xdg.join("phux")] {
        std::fs::create_dir_all(dir).expect("create plugin dirs");
    }
    std::fs::write(
        plugin.join("phux-plugin.toml"),
        "id = \"com.phux.test.delegation\"\nname = \"Delegation test agent\"\n\
         version = \"1.0.0\"\nmin_phux_version = \"0.0.2\"\nplatforms = [\"linux\", \"macos\"]\n",
    )
    .expect("write manifest");
    std::fs::write(
        integrations.join("deleg-agent.toml"),
        "schema_version = 1\nid = \"deleg-agent\"\ndisplay_name = \"Delegated agent\"\n\
         kind = \"terminal-agent\"\nfirst_party = true\n\n[launch]\n\
         command = [\"${PHUX_PLUGIN_ROOT}/scripts/agent.sh\"]\nworking_directory = \"workspace\"\n",
    )
    .expect("write integration");
    let agent = scripts.join("agent.sh");
    std::fs::write(&agent, script).expect("write agent script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&agent)
            .expect("stat agent script")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&agent, permissions).expect("make agent executable");
    }
    std::fs::write(
        xdg.join("phux/config.toml"),
        format!(
            "[[plugins]]\nmanifest = {:?}\nenabled = true\n",
            plugin.join("phux-plugin.toml").to_string_lossy()
        ),
    )
    .expect("write config");
    xdg
}

/// ADR-0163 §5: `agent start` runs the agent on a delegated credential. The
/// typed line points `PHUX_SOCKET` at the credential's own socket, so the
/// agent's phux commands can read its session but cannot kill a pane.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn agent_start_runs_the_agent_on_a_delegated_credential() {
    let server = ServerGuard::start("agent");
    let root = tempfile::tempdir().expect("create temp dir");
    let phux = crate::runner::phux_bin();
    let xdg = write_script_integration(
        root.path(),
        &format!(
            "#!/bin/sh\n\
             printf 'SOCK=%s\\n' \"$PHUX_SOCKET\"\n\
             if '{phux}' ls --json >/dev/null 2>&1; then echo LS=ok; else echo LS=refused; fi\n\
             if '{phux}' kill --yes @1 >/dev/null 2>&1; then echo KILL=ok; else echo KILL=refused; fi\n\
             exec sleep 60\n"
        ),
    );
    let out = common::phux_cmd(crate::runner::phux_bin())
        .env("XDG_CONFIG_HOME", &xdg)
        .env("XDG_STATE_HOME", root.path().join("state"))
        .args([
            "agent",
            "start",
            "--json",
            "--no-wait",
            "--force",
            "--kind",
            "deleg-agent",
            "--integration",
            "deleg-agent",
            "--target",
            "@1",
        ])
        .arg("--socket")
        .arg(&server.socket)
        .arg("bot")
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent start");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "agent start failed: {stderr}");

    let wait = common::phux_cmd(crate::runner::phux_bin())
        .args(["wait", "--until", "KILL=", "--timeout", "60", "--socket"])
        .arg(&server.socket)
        .arg("@1")
        .stdin(Stdio::null())
        .output()
        .expect("run phux wait");
    assert!(
        wait.status.success(),
        "the agent never reported: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    let screen = server.run_with_env(&["snapshot", "@1"], &[]);
    let sock = screen
        .lines()
        .find_map(|line| line.split_once("SOCK=").map(|(_, rest)| rest))
        .unwrap_or_else(|| panic!("no SOCK= line: {screen}"))
        .trim_end_matches(|ch: char| ch == '│' || ch.is_whitespace())
        .to_owned();
    assert!(!sock.is_empty(), "PHUX_SOCKET was not set: {screen}");
    assert_ne!(
        std::path::Path::new(&sock),
        server.socket.as_path(),
        "the agent must not run on the owner's socket"
    );
    assert!(
        screen.contains("LS=ok"),
        "the agent reads its session: {screen}"
    );
    assert!(
        screen.contains("KILL=refused"),
        "the agent cannot kill a pane: {screen}"
    );
}
