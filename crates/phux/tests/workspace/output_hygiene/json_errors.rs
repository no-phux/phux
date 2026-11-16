/// Run `phux <args...>` with an optional `XDG_CONFIG_HOME`, returning
/// `(exit_code, stdout, stderr)`.
fn run_maybe_xdg(args: &[&str], xdg: Option<&std::path::Path>) -> (i32, String, String) {
    xdg.map_or_else(|| run(args), |xdg| run_with_xdg(args, xdg))
}

/// The `no_server` contract, with the socket named in the message.
fn assert_no_server_json_contract(args: &[&str], xdg: Option<&std::path::Path>, sock: &str) {
    let doc = assert_json_error_contract(args, xdg, "no_server", 1);
    assert!(
        doc["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(sock)),
        "{args:?}: the message names the socket; got {doc}"
    );
}

/// Every dialing `--json` verb against an abandoned socket (bound, then
/// dropped: a clean `ECONNREFUSED`) follows the `no_server` contract.
/// Auto-spawning verbs (`new`) reap the stale socket instead.
#[test]
fn json_error_contract_holds_across_core_verbs_with_no_server() {
    let tmp = TempDir::new().expect("tempdir");
    let sock_file = tmp.path().join("dead.sock");
    drop(std::os::unix::net::UnixListener::bind(&sock_file).expect("bind then abandon socket"));
    assert!(sock_file.exists(), "the abandoned socket file must persist");
    let sock = sock_file.to_str().expect("utf-8 temp path");

    // `play` validates its cast before dialing; give it a real one.
    let cast = tmp.path().join("demo.cast");
    std::fs::write(
        &cast,
        "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"hi\"]\n",
    )
    .expect("write cast");
    let cast = cast.to_str().expect("utf-8 temp path");
    let out = tmp.path().join("out.cast");
    let out = out.to_str().expect("utf-8 temp path");

    // `launch` resolves its integration from config before dialing; the
    // checked-in demo plugin provides one.
    let demo_xdg = crate::runner::manifest_dir()
        .join("../../plugins/agent-tools/config")
        .canonicalize()
        .expect("demo plugin config");

    for args in [
        &["ls", "--json", "--socket", sock][..],
        &["snapshot", "--json", "work", "--socket", sock],
        &["wait", "--json", "--socket", sock, "work"],
        &["run", "--json", "--socket", sock, "work", "true"],
        &["watch", "--json", "--socket", sock, "work"],
        &["resize", "work", "80x24", "--json", "--socket", sock],
        &["spawn", "--json", "--socket", sock],
        &["play", cast, "--json", "--socket", sock],
        &["rec", "--json", "-o", out, "--socket", sock],
        &["ask", "work", "--json", "--socket", sock, "hello?"],
        &["tag", "ls", "work", "--json", "--socket", sock],
    ] {
        assert_no_server_json_contract(args, None, sock);
    }
    let launch = ["launch", "codex", "--json", "--socket", sock];
    assert_no_server_json_contract(&launch, Some(&demo_xdg), sock);
    assert!(
        !std::path::Path::new(out).exists(),
        "a capture that never started must not leave an empty cast behind"
    );
}

/// The verbs that run on the shared CLI context (`commands::ctx`) report
/// every refusal through one renderer, so under `--json` each refused path
/// is exactly ONE contract line on stderr: a dead server, a selector that
/// does not parse, a malformed id. Nothing here needs a live server.
#[test]
fn every_ctx_verb_reports_a_refusal_as_one_contract_line() {
    let tmp = TempDir::new().expect("tempdir");
    let sock_file = tmp.path().join("dead.sock");
    drop(std::os::unix::net::UnixListener::bind(&sock_file).expect("bind then abandon socket"));
    let sock = sock_file.to_str().expect("utf-8 temp path");
    let id = "0123456789abcdef0123456789abcdef";

    for args in [
        &["resize", "work", "80x24", "--json", "--socket", sock][..],
        &["tag", "add", "work", "ci", "--json", "--socket", sock],
        &["tag", "rm", "work", "ci", "--json", "--socket", sock],
        // KEYS is variadic: flags go before the target.
        &["send-keys", "--json", "--socket", sock, "work", "x"],
        &["paste", "work", "text", "--json", "--socket", sock],
        &["rename", "work", "other", "--json", "--socket", sock],
        &["take", "work", "--json", "--socket", sock],
        &["give", "work", "--json", "--socket", sock],
        &["signal", "work", "freeze", "--json", "--socket", sock],
        &["approvals", "--json", "--socket", sock],
        &["approve", id, "--yes", "--json", "--socket", sock],
        &["deny", id, "--json", "--socket", sock],
        &["detach", "--yes", "--json", "--socket", sock],
        &["whoami", "--json", "--socket", sock],
    ] {
        assert_no_server_json_contract(args, None, sock);
    }

    let bad = "work:1.x";
    for args in [
        &["resize", bad, "80x24", "--json", "--socket", sock][..],
        &["send-keys", "--json", "--socket", sock, bad, "x"],
        &["paste", bad, "text", "--json", "--socket", sock],
        &["take", bad, "--json", "--socket", sock],
        &["give", bad, "--json", "--socket", sock],
        &["signal", bad, "freeze", "--json", "--socket", sock],
    ] {
        let _ = assert_json_error_contract(args, None, "invalid_selector", 1);
    }
    for args in [
        &["approve", "not-an-id", "--yes", "--json", "--socket", sock][..],
        &["deny", "not-an-id", "--json", "--socket", sock],
    ] {
        let _ = assert_json_error_contract(args, None, "usage_error", 2);
    }
}

/// Without `--json` the same refusals keep the prose each verb has always
/// printed: the shared no-server block, and the one-line selector error.
#[test]
fn ctx_verbs_keep_their_prose_without_json() {
    let tmp = TempDir::new().expect("tempdir");
    let sock_file = tmp.path().join("dead.sock");
    drop(std::os::unix::net::UnixListener::bind(&sock_file).expect("bind then abandon socket"));
    let sock = sock_file.to_str().expect("utf-8 temp path");

    let (code, _stdout, stderr) = run(&["send-keys", "--socket", sock, "work", "x"]);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stderr.contains(sock) && stderr.contains("phux doctor"),
        "the no-server block names the socket and its remedies; got {stderr:?}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(stderr.trim()).is_err(),
        "{stderr:?}"
    );

    let (code, _stdout, stderr) = run(&["resize", "work:1.x", "80x24", "--socket", sock]);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stderr.contains("invalid target 'work:1.x'") && stderr.trim().lines().count() == 1,
        "a bad selector is one prose line; got {stderr:?}"
    );
}

/// Assert the per-verb contract: the expected exit code, empty stdout, and
/// ONE stderr line parsing as the contract document with the expected
/// `error.code`, matching `exit_code`, and a non-empty remedy.
fn assert_json_error_contract(
    args: &[&str],
    xdg: Option<&std::path::Path>,
    expected_code: &str,
    expected_exit: i32,
) -> serde_json::Value {
    let verb = args[0];
    let (code, stdout, stderr) = run_maybe_xdg(args, xdg);
    assert_eq!(
        code,
        expected_exit,
        "`phux {}` must exit {expected_exit}; stderr={stderr}",
        args.join(" ")
    );
    assert!(
        stdout.is_empty(),
        "`{verb} --json` failure must leave stdout empty; got {stdout:?}"
    );
    let line = stderr.trim();
    assert!(
        !line.contains('\n'),
        "`{verb} --json` failure must be ONE stderr line; got {stderr:?}"
    );
    let doc: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|err| {
        panic!("`{verb} --json` stderr must parse as JSON ({err}); got {stderr:?}")
    });
    assert_eq!(doc["schema_version"], 1, "{verb}: {doc}");
    assert_eq!(doc["error"]["code"], expected_code, "{verb}: {doc}");
    assert_eq!(doc["exit_code"], expected_exit, "{verb}: {doc}");
    assert!(
        doc["remedy"].as_str().is_some_and(|r| !r.is_empty()),
        "{verb}: the error must carry a non-empty remedy; got {doc}"
    );
    doc
}

/// A session name the selector grammar cannot address back (`x:y` reads as
/// window `y` of `x`) is refused before any server is dialed or spawned.
#[test]
fn new_refuses_an_unaddressable_session_name_before_dialing() {
    let socket = dead_socket();
    for name in ["x:y", "@3", ""] {
        let doc = assert_json_error_contract(
            &["new", "-s", name, "--json", "--socket", &socket],
            None,
            "invalid_session_name",
            2,
        );
        assert!(
            doc["error"]["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("invalid session name")),
            "{doc}"
        );
    }
    assert!(
        !std::path::Path::new(&socket).exists(),
        "a refused name must not auto-spawn a server"
    );
}

/// The registry table: one provocable local failure per `--json`-bearing
/// registry verb, each emitting the contract with its family's code.
#[test]
fn json_error_contract_holds_across_registry_verbs() {
    let tmp = TempDir::new().expect("tempdir");

    // An empty-but-valid config: id/name lookups miss cleanly.
    let empty_xdg = tmp.path().join("xdg-empty");
    std::fs::create_dir_all(empty_xdg.join("phux")).expect("create config dir");
    std::fs::write(empty_xdg.join("phux").join("config.toml"), "").expect("write config");

    // A config whose TOML does not parse: registry loads fail.
    let broken_xdg = tmp.path().join("xdg-broken");
    std::fs::create_dir_all(broken_xdg.join("phux")).expect("create config dir");
    std::fs::write(
        broken_xdg.join("phux").join("config.toml"),
        "not = [valid\n",
    )
    .expect("write broken config");
    let broken_toml = broken_xdg.join("phux").join("config.toml");
    let broken_toml = broken_toml.to_str().expect("utf-8 temp path");

    // A directory that is not inside any git repository.
    let not_repo = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&not_repo).expect("create dir");
    let not_repo = not_repo.to_str().expect("utf-8 temp path");

    // `tag`'s selector parse error needs no server at all; `config check`
    // keeps its distinct exit 2 for "could not check at all".
    let cases: [(&[&str], Option<&std::path::Path>, &str, i32); 6] = [
        (&["tag", "ls", "work:1.x", "--json"], None, "invalid_selector", 1),
        (&["plugin", "unlink", "no.such.plugin", "--json"], Some(&empty_xdg), "registry", 1),
        (&["host", "ls", "--json"], Some(&broken_xdg), "registry", 1),
        (&["worktree", "list", not_repo, "--json"], None, "workspace", 1),
        (&["project", "inspect", not_repo, "--json"], None, "workspace", 1),
        (&["config", "check", broken_toml, "--json"], Some(&empty_xdg), "invalid_config", 2),
    ];
    for (args, xdg, code, exit) in cases {
        let _ = assert_json_error_contract(args, xdg, code, exit);
    }
}

/// The registry aliases take the SAME failure paths as their canonical
/// spellings: `plugin rm` under `--json` emits the identical contract line
/// `plugin unlink` does.
#[test]
fn alias_spellings_share_the_canonical_failure_paths() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(xdg.join("phux")).expect("create config dir");
    std::fs::write(xdg.join("phux").join("config.toml"), "").expect("write config");

    let canonical = run_with_xdg(&["plugin", "unlink", "no.such.plugin", "--json"], &xdg);
    let alias = run_with_xdg(&["plugin", "rm", "no.such.plugin", "--json"], &xdg);
    assert_eq!(
        canonical, alias,
        "an alias is a second name, never a second code path"
    );
}
/// `doctor --json` on a failure exit: the verdict is the stdout document and
/// stderr is empty (provoked with a socket path too long for `sockaddr_un`).
#[test]
fn doctor_json_failure_exit_is_json_only() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(xdg.join("phux")).expect("create config dir");
    std::fs::write(xdg.join("phux").join("config.toml"), "").expect("write config");

    let long_sock = format!("/tmp/{}/phux.sock", "x".repeat(200));
    let (code, stdout, stderr) = run_with_xdg(&["doctor", "--json", "--socket", &long_sock], &xdg);

    assert_eq!(code, 1, "a failed check fails the run; stderr={stderr}");
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("`doctor --json` stdout must be one JSON document");
    assert_eq!(doc["ok"], false);
    assert!(
        doc["checks"]
            .as_array()
            .is_some_and(|checks| checks.iter().any(|c| c["status"] == "fail")),
        "the failing check must be in the document; got {doc}"
    );
    assert!(
        stderr.trim().is_empty(),
        "`doctor --json` must never print prose on a failure exit; got {stderr:?}"
    );
}

/// The same failure without `--json` stays prose: unparseable as JSON,
/// multi-line, naming the missing server and its remedies (the shape the
/// `no_server_lines` unit tests pin exactly).
#[test]
fn no_server_without_json_stays_prose() {
    let sock = dead_socket();
    let (code, _stdout, stderr) = run(&["ls", "--socket", &sock]);
    assert_eq!(code, 1);
    assert!(
        serde_json::from_str::<serde_json::Value>(stderr.trim()).is_err(),
        "prose path must not emit the JSON document; got {stderr:?}"
    );
    assert!(
        stderr.contains("no server running") && stderr.contains("phux doctor"),
        "prose keeps the remedy block; got {stderr:?}"
    );
    assert!(!stderr.contains(BANNER_FRAGMENT), "stderr={stderr:?}");
}

/// `resize --json` reports a refusal on the contract, not as prose: the
/// pane resolves on the first connection and is gone from the read-back on
/// the second, which `resize_to` refuses. Exactly one JSON stderr line.
#[test]
fn resize_json_reports_a_refusal_as_one_contract_line() {
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

    let snapshot = |resources: Vec<ResourceInfo>| {
        let (session, window) = (SessionId::new(1), WindowId::new(1));
        SessionSnapshot::new(session, window, ResourceId::local(7))
            .with_sessions(vec![SessionInfo::new(session, "work")])
            .with_windows(vec![WindowInfo::new(window, session, "shell")])
            .with_resources(resources)
    };
    let present = snapshot(vec![ResourceInfo::new(
        ResourceId::local(7),
        WindowId::new(1),
        80,
        24,
    )]);
    let gone = snapshot(Vec::new());

    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("phux.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener).expect("listener");
            ScriptedServer::accept(&listener, ScriptSpec::new().state(present)).await;
            ScriptedServer::accept(&listener, ScriptSpec::new().state(gone)).await;
        });
    });

    let sock = socket.to_str().expect("utf-8 temp path");
    let doc = assert_json_error_contract(
        &["resize", "@7", "100x30", "--json", "--socket", sock],
        Some(tmp.path()),
        "transport",
        1,
    );
    assert!(
        doc["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("cannot resize '@7': ")),
        "{doc}"
    );
    server.join().expect("scripted server");
}
