/// Write an executable fake Claude named `claude` (the kind is identified
/// from the foreground process name) that paints the permission-dialog shape
/// Claude Code actually draws (`phux-agent-rules` fixture
/// `claude/blocked_permission.txt`: transcript above a rule, the dialog stem and
/// numbered options below) and then holds.
fn write_fake_claude(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("claude");
    let script = concat!(
        "#!/bin/sh\n",
        "printf '\\033[2J\\033[H'\n",
        "echo 'some transcript output above the live chrome'\n",
        "echo ''\n",
        "printf '\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200",
        "\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200",
        "\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200",
        "\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\n'\n",
        "echo ' Bash command'\n",
        "echo ''\n",
        "echo '   touch /tmp/probe.txt'\n",
        "echo ''\n",
        "echo ' Do you want to proceed?'\n",
        "printf ' \\342\\235\\257 1. Yes\\n'\n",
        "echo '   2. Yes, and always allow access'\n",
        "echo '   3. No'\n",
        "echo ''\n",
        "echo ' Esc to cancel'\n",
        "sleep 120\n",
    );
    std::fs::write(&path, script).expect("write fake claude");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake claude");
    path
}

/// Run the installed wrapper's hook entry point (`<shim> --phux-hook
/// <event>`) with a pane's environment. `PHUX_AGENT_PHUX_BIN` is unset: the
/// wrapper must reach the binary baked in at install.
fn run_hook(shim: &std::path::Path, event: &str, terminal_id: u32, socket: &std::path::Path) {
    let out = common::phux_cmd(shim)
        .args(["--phux-hook", event])
        .env("PHUX_TERMINAL_ID", terminal_id.to_string())
        .env("PHUX_SOCKET", socket)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|err| panic!("run the installed shim's {event} hook: {err}"));
    // The wrapper swallows `phux` failures on purpose — a broken control
    // plane must never break the user's Claude — so a zero exit proves only
    // that the hook path ran. What it actually did is read off the record.
    assert!(
        out.status.success(),
        "the {event} hook exited {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The wrapper `agent install-claude` generates, run through its hook entry
/// against a live server, leaves the ADR-0046 detector armed (the unit tests
/// only pinned the rendered string and a hand-written identity-only write):
///
/// 1. after `SessionStart` the pane reaches `blocked`, a state only
///    `rules/claude.toml` can produce;
/// 2. repeated `blocked` hooks do not clobber it (a wholesale record write
///    would publish `blocked -> unknown`, read by `agent wait` as departure);
/// 3. counterfactual: declaring a state, as the old shim did, stands the
///    detector down on the same screen.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn the_generated_claude_shim_leaves_the_detector_armed_on_a_live_pane() {
    let home = tempfile::tempdir().expect("create temp HOME");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin dir");
    let fake_claude = write_fake_claude(&bin);
    let data = home.path().join("data");

    // Install through the real verb, so what runs below is the shipped
    // generator's output and not a fixture that resembles it.
    let install = common::phux_cmd(crate::runner::phux_bin())
        .args(["agent", "install-claude", "--shell", "bash", "--real"])
        .arg(&fake_claude)
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", &data)
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent install-claude");
    assert!(
        install.status.success(),
        "install-claude exited {:?}; stderr={}",
        install.status.code(),
        String::from_utf8_lossy(&install.stderr)
    );
    let shim = data.join("phux").join("shims").join("claude");
    let installed = std::fs::read_to_string(&shim).expect("read the installed wrapper");
    // The staleness stamp `phux doctor` keys on; `--state` is `shim.rs`'s unit
    // test, not this one's.
    assert!(
        installed.contains("# phux-shim-schema: "),
        "the installed wrapper must carry its behavior stamp:\n{installed}"
    );

    let server = start_with_env(&[("PHUX_AGENT_STARTUP_GRACE_MS", TEST_STARTUP_GRACE_MS)]);
    let terminal_id = server.spawn_pane(&fake_claude);
    let target = format!("@{terminal_id}");

    // --- 1. SessionStart: identity only, and the detector fills state in ---
    run_hook(&shim, "start", terminal_id, &server.socket);
    let json = server.await_agent_state(&target, "blocked", DETECT_DEADLINE);
    let agent = &json["agents"][0];
    assert_eq!(
        agent["sources"][0]["kind"], "agent_record",
        "the shim's write must be the authority the report reads: {json}"
    );
    assert_eq!(
        agent["agent"]["label"], "claude",
        "the shim's name survives the detector's state write: {json}"
    );
    assert_eq!(
        agent["agent"]["kind"], "claude",
        "and so does its kind: {json}"
    );

    // --- 2. The per-hook `blocked` write must not clobber the derived state -
    for _ in 0..3 {
        run_hook(&shim, "blocked", terminal_id, &server.socket);
    }
    let observed = server.states_over(&target, HOLD_WINDOW);
    assert_eq!(
        observed,
        vec!["blocked".to_owned()],
        "a per-hook record write resets the derived state to `unknown`, which \
         `agent wait` reads as the agent departing (phux-w7z2.37)"
    );

    // --- 3. The counterfactual: schema 1's declaration disarms the detector -
    server.agent(&[
        "set", &target, "--name", "claude", "--kind", "claude", "--state", "idle",
    ]);
    let observed = server.states_over(&target, HOLD_WINDOW);
    assert_eq!(
        observed,
        vec!["idle".to_owned()],
        "a declared state outranks the detector (ADR-0046 point 8), so the pane \
         sits on the declaration while its screen still shows a live permission \
         dialog — this is exactly what the shipped shim used to do on every hook, \
         and it is what phase 1 proves the generated wrapper no longer does"
    );
}
