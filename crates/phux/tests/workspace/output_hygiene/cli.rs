#[test]
fn retired_cockpit_command_and_log_flags_are_usage_errors() {
    for args in [
        &["cockpit"][..],
        &["cockpit", "--json"][..],
        &["logs", "--cockpit"][..],
        &["logs", "--cockpit", "-f", "-n", "50"][..],
        &["logs", "--json", "--cockpit"][..],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(stderr.contains("cockpit"), "{args:?}: {stderr}");
    }
}

#[test]
fn help_and_capabilities_do_not_advertise_retired_cockpit() {
    for args in [
        &["--help"][..],
        &["logs", "--help"][..],
        &["update", "--help"][..],
        &["channel", "--help"][..],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, 0, "{args:?}: {stderr}");
        assert!(
            !stdout.to_lowercase().contains("cockpit"),
            "{args:?}: {stdout}"
        );
    }

    let (code, stdout, stderr) = run(&["--capabilities", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("capabilities JSON");
    // Companion executable paths may contain "cockpit" (including this
    // worktree's name). Only product command/schema declarations advertise it.
    for key in ["commands", "json_contracts"] {
        assert!(!doc[key].to_string().contains("cockpit"), "{key}: {doc}");
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
fn version_is_clean_stdout_with_no_banner() {
    let (code, stdout, stderr) = run(&["--version"]);
    assert_eq!(code, 0, "--version should exit 0; stderr={stderr}");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "--version stdout should carry the version; got {stdout:?}"
    );
    assert!(
        !stderr.contains(BANNER_FRAGMENT),
        "--version must not print the banner to stderr; stdout={stdout:?} stderr={stderr:?}"
    );
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
fn skill_flag_refuses_a_command_as_an_invalid_scope() {
    let (code, stdout, stderr) = run(&["--skill", "ls"]);

    assert_eq!(code, 2);
    assert!(stdout.is_empty(), "stdout={stdout:?}");
    assert!(stderr.contains("invalid value 'ls'"));
    assert!(stderr.contains("quick"));
    assert!(stderr.contains("terminal"));
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

#[test]
fn short_and_long_help_progressively_disclose_the_root() {
    let (short_code, short, short_err) = run(&["-h"]);
    let (long_code, long, long_err) = run(&["--help"]);
    assert_eq!(short_code, 0, "short help failed: {short_err}");
    assert_eq!(long_code, 0, "long help failed: {long_err}");
    assert!(short.contains("Start here:"), "short help:\n{short}");
    assert!(short.contains("phux                     Attach"));
    assert!(short.contains("phux --skill"));
    assert!(!short.contains("Sessions:"), "short help:\n{short}");
    assert!(long.contains("Sessions:"), "long help:\n{long}");
    assert!(
        !long_err.contains(BANNER_FRAGMENT),
        "--help printed the banner"
    );
    assert!(long.contains("Learn more:"), "long help:\n{long}");
    // Grouped rows are laid out in columns; compare on whitespace-collapsed text.
    let flat = long.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("spawn Create a pane"),
        "long help must list spawn:\n{long}"
    );
    assert!(
        flat.contains("launch Start an agent integration"),
        "long help must list launch:\n{long}"
    );
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

/// A verb that never dials a server refuses a provided `--socket` with a
/// one-line teaching error instead of silently ignoring it.
#[test]
fn socketless_verb_rejects_socket_with_teaching_error() {
    let sock = dead_socket();
    for args in [
        vec!["config", "path", "--socket", sock.as_str()],
        vec!["pair", "ls", "--socket", sock.as_str()],
        vec!["--socket", sock.as_str(), "plugin", "list"],
    ] {
        let (code, stdout, stderr) = run(&args);
        assert_eq!(
            code,
            2,
            "`phux {}` must refuse --socket as a usage error; stderr={stderr}",
            args.join(" ")
        );
        assert!(stdout.is_empty(), "refusals leave stdout empty");
        assert!(
            stderr.contains("--socket") && stderr.contains("never dials a server"),
            "the refusal must teach why; got {stderr:?}"
        );
    }
}

/// A scoped JSON flag placed before the verb gets a usage document whose
/// remedy teaches the fix.
#[test]
fn misplaced_json_before_verb_teaches_placement() {
    let (code, stdout, stderr) = run(&["--json", "ls"]);
    assert_eq!(code, 2, "a misplaced --json is a usage error");
    assert!(stdout.is_empty());
    assert_eq!(stderr.lines().count(), 1);
    let doc: serde_json::Value = serde_json::from_str(&stderr).expect("usage error document");
    assert_eq!(
        doc["error"]["message"],
        "unexpected argument '--json' found"
    );
    assert_eq!(
        doc["remedy"],
        "`--json` is set per verb, not on `phux` itself; place it after the verb: `phux <verb> --json ...`"
    );
}

/// A root `--rec` in front of a verb is refused with the two correct
/// spellings named (the `args_conflicts_with_subcommands` replacement).
#[test]
fn root_rec_before_verb_teaches_the_two_spellings() {
    let (code, stdout, stderr) = run(&["--rec", "demo.gif", "ls"]);
    assert_eq!(code, 2, "a root --rec before a verb is a usage error");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("phux attach --rec") && stderr.contains("phux rec"),
        "the refusal must name the attach and headless spellings; got {stderr:?}"
    );
}

/// `phux logs --json` (the inventory) is pure JSON on stdout — no banner,
/// no prose mixed into the document channel — and exits 0 even on a fresh
/// machine where nothing exists yet.
#[test]
fn logs_json_inventory_is_pure_json_stdout() {
    let (code, stdout, stderr) = run(&["logs", "--json"]);
    assert_eq!(code, 0, "the inventory always answers; stderr={stderr}");
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("`logs --json` stdout must be one JSON document");
    assert_eq!(doc["schema_version"], 2);
    assert!(doc.get("cockpit_log").is_none());
    assert!(
        !stderr.contains(BANNER_FRAGMENT),
        "`logs --json` must not print the banner; got {stderr:?}"
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
