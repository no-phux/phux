/// Never dialed: the verbs below refuse `--socket` before any connect.
const SOCKETLESS_SOCKET: &str = "/tmp/phux-output-hygiene-never-dialed.sock";

/// Usage refusals: each `args` exits 2 with empty stdout and every needle
/// on stderr.
const USAGE_REFUSALS: &[(&[&str], &[&str])] = &[
    // `--skill` takes a scope, never a command.
    (&["--skill", "ls"], &["invalid value 'ls'", "quick", "terminal"]),
    // A root `--rec` in front of a verb names the two correct spellings
    // (the `args_conflicts_with_subcommands` replacement).
    (
        &["--rec", "demo.gif", "ls"],
        &["phux attach --rec", "phux rec"],
    ),
    // A verb that never dials a server refuses a provided `--socket` with a
    // one-line teaching error instead of silently ignoring it.
    (
        &["config", "path", "--socket", SOCKETLESS_SOCKET],
        &["--socket", "never dials a server"],
    ),
    (
        &["pair", "ls", "--socket", SOCKETLESS_SOCKET],
        &["--socket", "never dials a server"],
    ),
    (
        &["--socket", SOCKETLESS_SOCKET, "plugin", "list"],
        &["--socket", "never dials a server"],
    ),
];

#[test]
fn usage_refusals_exit_2_with_their_teaching_message() {
    for (args, needles) in USAGE_REFUSALS {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, 2, "`phux {}`: stderr={stderr}", args.join(" "));
        assert!(stdout.is_empty(), "{args:?}: refusals leave stdout empty: {stdout}");
        for needle in *needles {
            assert!(stderr.contains(needle), "{args:?}: missing {needle:?}; got {stderr:?}");
        }
    }
}

#[test]
fn redirected_interactive_invocations_do_not_spawn_or_emit_terminal_bytes() {
    for args in [&[][..], &["attach"][..], &["new", "redirected"][..]] {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("phux.sock");
        let state = dir.path().join("state");
        let out = phux()
            .args(args)
            .args(["--socket"])
            .arg(&socket)
            .env("XDG_STATE_HOME", &state)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run redirected interactive invocation");

        assert!(!out.status.success(), "redirected {args:?} must fail");
        assert_eq!(out.stdout, b"", "redirected {args:?} emitted stdout bytes");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("interactive use requires both stdin and stdout to be terminals"),
            "redirected {args:?} did not explain the TTY requirement: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !socket.exists(),
            "redirected {args:?} created a server socket at {}",
            socket.display()
        );
        assert!(
            !state.exists(),
            "redirected {args:?} created client state before refusing the TTY"
        );
    }
}

#[test]
fn redirected_worktree_attach_refuses_before_git_mutation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("created-worktree");
    let out = phux()
        .args(["worktree", "new", "review", "--repo"])
        .arg(dir.path())
        .args(["--path"])
        .arg(&worktree)
        .arg("--attach")
        .stdin(Stdio::null())
        .output()
        .expect("run redirected worktree attach");

    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("interactive use requires both stdin and stdout to be terminals")
    );
    assert!(!worktree.exists(), "TTY refusal came after git mutation");
}

#[test]
fn redirected_remote_attaches_refuse_before_dialing() {
    for args in [
        ["attach", "--ws", "ws://127.0.0.1:9"].as_slice(),
        ["attach", "--quic", "127.0.0.1:9"].as_slice(),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("state");
        let out = phux()
            .args(args)
            .env("XDG_STATE_HOME", &state)
            .stdin(Stdio::null())
            .output()
            .expect("run redirected remote attach");
        assert!(!out.status.success());
        assert_eq!(out.stdout, b"");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "phux: error: interactive use requires both stdin and stdout to be terminals\n"
        );
        assert!(!state.exists(), "remote attach initialized client state");
    }
}

#[test]
fn telemetry_failure_does_not_contaminate_json_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = phux()
        .args(["ls", "--json", "--socket", &dead_socket()])
        .env("PHUX_LOG", dir.path())
        .output()
        .expect("run JSON command with invalid log destination");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<_> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "JSON stderr was contaminated: {stderr:?}");
    serde_json::from_str::<serde_json::Value>(lines[0]).expect("stderr is one JSON document");
}

#[test]
fn skill_flag_is_clean_and_matches_the_legacy_verb() {
    let (flag_code, flag_stdout, flag_stderr) = run(&["--skill"]);
    let (verb_code, verb_stdout, verb_stderr) = run(&["skill"]);
    let (_, explicit_flag, _) = run(&["--skill=full"]);
    let (_, explicit_verb, _) = run(&["skill", "full"]);

    assert_eq!(flag_code, 0, "--skill should exit 0; stderr={flag_stderr}");
    assert_eq!(verb_code, 0, "skill should exit 0; stderr={verb_stderr}");
    assert_eq!(flag_stdout, verb_stdout);
    assert_eq!(flag_stdout, explicit_flag);
    assert_eq!(flag_stdout, explicit_verb);
    assert!(flag_stdout.starts_with("---\nname: using-phux\n"));
    assert!(flag_stdout.ends_with('\n'));
    assert!(flag_stderr.is_empty(), "--skill stderr={flag_stderr:?}");
    assert!(verb_stderr.is_empty(), "skill stderr={verb_stderr:?}");

    for scope in ["quick", "agent", "terminal"] {
        let (_, flag, flag_err) = run(&[&format!("--skill={scope}")]);
        let (_, verb, verb_err) = run(&["skill", scope]);
        assert_eq!(flag, verb, "scope={scope}");
        assert!(flag.starts_with("---\nname: using-phux\n"));
        assert!(!flag.contains("phux-skill-region:"));
        assert!(flag_err.is_empty() && verb_err.is_empty());
    }
}

#[test]
fn capabilities_are_clean_machine_readable_and_socketless() {
    let (code, stdout, stderr) = run(&["--capabilities", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stderr.is_empty());
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["binary"]["name"], "phux");
    assert_eq!(
        doc["skill"]["scopes"],
        serde_json::json!(["quick", "agent", "terminal", "full"])
    );
    assert!(
        doc["commands"]
            .as_array()
            .is_some_and(|commands| !commands.is_empty())
    );

    let socket = dead_socket();
    let (code, stdout, stderr) = run(&["--capabilities", "--json", "--socket", &socket]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert!(stderr.contains("standalone endpoint"));
}

/// `phux --socket X ls` and `phux ls --socket X` are the same invocation:
/// same exit code, same stdout, same stderr. Pointed at a dead socket so
/// both take the identical "no server" path.
#[test]
fn socket_before_and_after_the_verb_behave_identically() {
    let sock = dead_socket();
    let before = run(&["--socket", &sock, "ls"]);
    let after = run(&["ls", "--socket", &sock]);
    assert_eq!(
        before, after,
        "the two --socket positions must be indistinguishable"
    );
    assert_ne!(before.0, 0, "no server means a nonzero exit");
    assert!(
        before.2.contains("no server"),
        "the shared failure names the missing server; got {:?}",
        before.2
    );
}

#[test]
fn server_and_client_logs_still_tail_without_a_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("phux-logs-test");
    std::fs::create_dir(&state).expect("create log directory");
    std::fs::write(state.join("server.log"), "server first\nserver last\n").unwrap();
    std::fs::write(state.join("client-42.log"), "client first\nclient last\n").unwrap();

    for (args, expected) in [
        (&["logs", "--server", "-n", "1"][..], "server last\n"),
        (&["logs", "--client", "-n", "1"][..], "client last\n"),
        (
            &["logs", "--client", "--pid", "42", "-n", "1"][..],
            "client last\n",
        ),
    ] {
        let out = phux()
            .args(args)
            .env("PHUX_PROFILE", "logs-test")
            .env("XDG_STATE_HOME", dir.path())
            .output()
            .expect("tail log");
        assert!(out.status.success(), "{args:?}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{args:?}");
    }
}
#[test]
fn config_path_is_one_clean_line() {
    let (code, stdout, stderr) = run(&["config", "path"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stderr.contains(BANNER_FRAGMENT), "stderr={stderr:?}");
    assert!(
        stdout.lines().count() == 1 && !stdout.trim().is_empty(),
        "{stdout:?}"
    );
}
