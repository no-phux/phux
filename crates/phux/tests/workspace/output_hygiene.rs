//! Output-hygiene contracts of the real binary, with no server (dead socket
//! paths, so nothing auto-spawns): one-shot verbs print no banner, `--json`
//! puts only JSON on stdout and errors on stderr, `--version` is clean stdout,
//! and a closed reader (`| head`) exits 0 silently. Every spawn goes through
//! [`phux`], which disables overlay detection so `doctor` never dials a real
//! tailnet server.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::process::{Command, Stdio};

use tempfile::TempDir;

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// A socket path that does not exist, so no verb finds (or spawns) a server.
fn dead_socket() -> String {
    format!("/tmp/phux-no-such-server-{}.sock", std::process::id())
}

/// A nonexistent `$PHUX_TAILSCALE` CLI: turns overlay detection (and the
/// CGNAT heuristic) off, and a failed `execve` cannot be slow.
const NO_OVERLAY_CLI: &str = "/nonexistent/phux-output-hygiene-no-overlay";

/// The binary under test, with overlay detection off.
fn phux() -> Command {
    let mut cmd = Command::new(PHUX);
    cmd.env("PHUX_TAILSCALE", NO_OVERLAY_CLI);
    cmd
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
            "phux: interactive use requires both stdin and stdout to be terminals\n"
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

/// Run `phux <args...>` and return `(exit_code, stdout, stderr)`.
fn run(args: &[&str]) -> (i32, String, String) {
    let out = phux().args(args).output().expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_with_xdg(args: &[&str], xdg_config_home: &std::path::Path) -> (i32, String, String) {
    let out = phux()
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The build banner; no one-shot verb may print it.
const BANNER_FRAGMENT: &str = concat!("phux ", env!("CARGO_PKG_VERSION"));

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
        vec!["pair", "--socket", sock.as_str()],
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

/// A scoped flag placed before the verb gets clap's refusal PLUS the
/// teaching hint naming the fix.
#[test]
fn misplaced_json_before_verb_teaches_placement() {
    let (code, stdout, stderr) = run(&["--json", "ls"]);
    assert_eq!(code, 2, "a misplaced --json is a usage error");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("hint:") && stderr.contains("--json") && stderr.contains("after the verb"),
        "the refusal must carry the placement hint; got {stderr:?}"
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

#[test]
fn config_plugins_json_is_machine_readable() {
    let tmp = TempDir::new().expect("tempdir");
    let plugin_dir = tmp.path().join("plugin");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    let manifest = plugin_dir.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        r#"
id = "example.agent-tools"
name = "Agent Tools"
version = "0.1.0"
min_phux_version = "0.0.2"

[[actions]]
id = "summarize"
title = "Summarize"
command = ["sh", "-c", "printf summarize"]

[[agents]]
id = "codex"
label = "Codex"
state = "blocked"
attention = "high"
contexts = ["workspace"]

[[panes]]
id = "board"
title = "Board"
command = ["true"]

[[workspaces]]
id = "agent-bench"
title = "Agent Bench"
contexts = ["workspace"]
agents = ["codex"]
actions = ["summarize"]

[[workspaces.panes]]
id = "board"
pane = "board"
role = "monitor"
"#,
    )
    .expect("write manifest");

    let config_dir = tmp.path().join("xdg").join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"
[[plugins]]
manifest = "{}"
enabled = true
"#,
            manifest.display()
        ),
    )
    .expect("write config");

    let (code, stdout, stderr) =
        run_with_xdg(&["config", "plugins", "--json"], &tmp.path().join("xdg"));

    assert_eq!(
        code, 0,
        "`config plugins --json` should exit 0; stderr={stderr}"
    );
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "`config plugins --json` must not print the banner; stdout={stdout:?} stderr={stderr:?}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["plugins"][0]["id"], "example.agent-tools");
    assert_eq!(value["plugins"][0]["actions"][0]["id"], "summarize");
    assert_eq!(value["plugins"][0]["agents"][0]["id"], "codex");
    assert_eq!(value["plugins"][0]["agents"][0]["state"], "blocked");
    assert_eq!(value["plugins"][0]["workspaces"][0]["id"], "agent-bench");
    assert_eq!(
        value["plugins"][0]["workspaces"][0]["panes"][0]["role"],
        "monitor"
    );
    assert_eq!(value["plugins"][0]["enabled"], true);
}

#[test]
fn config_plugins_json_resolves_relative_manifest_paths() {
    let tmp = TempDir::new().expect("tempdir");
    let config_dir = tmp.path().join("xdg").join("phux");
    let plugin_dir = config_dir.join("plugins").join("agent-tools");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    std::fs::write(
        plugin_dir.join("phux-plugin.toml"),
        r#"
id = "example.relative"
name = "Relative"
version = "0.1.0"
min_phux_version = "0.0.2"

[[actions]]
id = "open"
title = "Open"
command = ["true"]
"#,
    )
    .expect("write manifest");
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[[plugins]]
manifest = "./plugins/agent-tools/phux-plugin.toml"
enabled = false
"#,
    )
    .expect("write config");

    let (code, stdout, stderr) =
        run_with_xdg(&["config", "plugins", "--json"], &tmp.path().join("xdg"));

    assert_eq!(
        code, 0,
        "`config plugins --json` should resolve relative manifests; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["plugins"][0]["id"], "example.relative");
    assert_eq!(value["plugins"][0]["actions"][0]["id"], "open");
    assert_eq!(value["plugins"][0]["enabled"], false);
}

/// Run `phux <args...>` with an optional `XDG_CONFIG_HOME`, returning
/// `(exit_code, stdout, stderr)`.
fn run_maybe_xdg(args: &[&str], xdg: Option<&std::path::Path>) -> (i32, String, String) {
    xdg.map_or_else(|| run(args), |xdg| run_with_xdg(args, xdg))
}

/// The `no_server` contract, with the socket named in the message.
fn assert_no_server_json_contract(
    verb: &str,
    args: &[&str],
    xdg: Option<&std::path::Path>,
    sock: &str,
) {
    let doc = assert_json_error_contract(verb, args, xdg, "no_server", 1);
    assert!(
        doc["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(sock)),
        "{verb}: the message names the socket; got {doc}"
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
    let demo_xdg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/plugins/agent-tools/config")
        .canonicalize()
        .expect("demo plugin config");

    let cases: &[(&str, Vec<&str>, Option<&std::path::Path>)] = &[
        ("ls", vec!["ls", "--json", "--socket", sock], None),
        (
            "snapshot",
            vec!["snapshot", "--json", "work", "--socket", sock],
            None,
        ),
        (
            "wait",
            vec!["wait", "--json", "--socket", sock, "work"],
            None,
        ),
        (
            "run",
            vec!["run", "--json", "--socket", sock, "work", "true"],
            None,
        ),
        (
            "watch",
            vec!["watch", "--json", "--socket", sock, "work"],
            None,
        ),
        (
            "resize",
            vec!["resize", "work", "80x24", "--json", "--socket", sock],
            None,
        ),
        ("spawn", vec!["spawn", "--json", "--socket", sock], None),
        (
            "launch",
            vec!["launch", "codex", "--json", "--socket", sock],
            Some(&demo_xdg),
        ),
        ("play", vec!["play", cast, "--json", "--socket", sock], None),
        (
            "rec",
            vec!["rec", "--json", "-o", out, "--socket", sock],
            None,
        ),
        (
            "ask",
            vec!["ask", "work", "--json", "--socket", sock, "hello?"],
            None,
        ),
        (
            "tag",
            vec!["tag", "ls", "work", "--json", "--socket", sock],
            None,
        ),
    ];

    for (verb, args, xdg) in cases {
        assert_no_server_json_contract(verb, args, *xdg, sock);
    }
    assert!(
        !std::path::Path::new(out).exists(),
        "a capture that never started must not leave an empty cast behind"
    );
}

/// Assert the per-verb contract: the expected exit code, empty stdout, and
/// ONE stderr line parsing as the contract document with the expected
/// `error.code`, matching `exit_code`, and a non-empty remedy.
fn assert_json_error_contract(
    verb: &str,
    args: &[&str],
    xdg: Option<&std::path::Path>,
    expected_code: &str,
    expected_exit: i32,
) -> serde_json::Value {
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

/// One registry-table case: (verb, argv, `XDG_CONFIG_HOME` override,
/// expected `error.code`, expected exit code).
type RegistryCase<'a> = (
    &'a str,
    Vec<&'a str>,
    Option<&'a std::path::Path>,
    &'a str,
    i32,
);

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

    let cases: &[RegistryCase<'_>] = &[
        // `tag`'s selector parse error needs no server at all.
        (
            "tag",
            vec!["tag", "ls", "work:1.x", "--json"],
            None,
            "invalid_selector",
            1,
        ),
        (
            "plugin",
            vec!["plugin", "unlink", "no.such.plugin", "--json"],
            Some(&empty_xdg),
            "registry",
            1,
        ),
        (
            "host",
            vec!["host", "ls", "--json"],
            Some(&broken_xdg),
            "registry",
            1,
        ),
        (
            "worktree",
            vec!["worktree", "list", not_repo, "--json"],
            None,
            "workspace",
            1,
        ),
        (
            "workspace",
            vec!["workspace", "inspect", not_repo, "--json"],
            None,
            "workspace",
            1,
        ),
        // `config check` keeps its distinct exit 2 for "could not check at
        // all", in the document and the process alike.
        (
            "config check",
            vec!["config", "check", broken_toml, "--json"],
            Some(&empty_xdg),
            "invalid_config",
            2,
        ),
    ];

    for (verb, args, xdg, code, exit) in cases {
        let _ = assert_json_error_contract(verb, args, *xdg, code, *exit);
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

/// `phux logs --json` (the inventory) is pure JSON on stdout — no banner,
/// no prose mixed into the document channel — and exits 0 even on a fresh
/// machine where nothing exists yet.
#[test]
fn logs_json_inventory_is_pure_json_stdout() {
    let (code, stdout, stderr) = run(&["logs", "--json"]);
    assert_eq!(code, 0, "the inventory always answers; stderr={stderr}");
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("`logs --json` stdout must be one JSON document");
    assert_eq!(doc["schema_version"], 1);
    assert!(
        !stderr.contains(BANNER_FRAGMENT),
        "`logs --json` must not print the banner; got {stderr:?}"
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

/// Run `phux <args...>` with the read end of its real stdout pipe already
/// closed; returns `(exit code, None if killed by a signal; stderr)`.
fn run_with_closed_stdout(args: &[&str]) -> (Option<i32>, String) {
    let mut child = phux()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phux binary");

    drop(child.stdout.take().expect("child stdout is piped"));

    let out = child.wait_with_output().expect("wait for phux");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Assert the whole contract for one verb: exit 0, no panic, and no line
/// blaming a server that is not even running.
fn assert_survives_closed_stdout(args: &[&str]) {
    let (code, stderr) = run_with_closed_stdout(args);
    assert_eq!(
        code,
        Some(0),
        "`phux {}` with a closed stdout must exit 0 (None = died from a signal); stderr={stderr}",
        args.join(" ")
    );
    assert!(
        !stderr.contains("panicked"),
        "`phux {}` panicked on a closed stdout; stderr={stderr}",
        args.join(" ")
    );
    assert!(
        !stderr.contains("server panic"),
        "`phux {}` blamed the server for its own death; stderr={stderr}",
        args.join(" ")
    );
    assert!(
        stderr.is_empty(),
        "`phux {}` must hang up in silence; stderr={stderr}",
        args.join(" ")
    );
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

/// A hung-up reader must end each shape of stdout writer silently: a
/// ~160 KB completion script (guaranteed to hit the closed pipe), an `out!`
/// fragment, a `--json` document, a verb that dials first, and the parser's
/// own `--version` / `--help` / `--skill` output.
#[test]
fn every_writer_survives_a_closed_stdout() {
    let sock = dead_socket();
    for args in [
        vec!["completion", "bash"],
        vec!["config", "show", "--default"],
        vec!["config", "plugins", "--json"],
        vec!["doctor", "--socket", &sock],
        vec!["--version"],
        vec!["--help"],
        vec!["--skill"],
    ] {
        assert_survives_closed_stdout(&args);
    }
}
