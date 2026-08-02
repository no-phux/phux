//! Binary-level output-hygiene tests for `phux` (cli-ergonomics).
//!
//! These drive the REAL `phux` binary (handed to us by cargo at
//! `env!("CARGO_BIN_EXE_phux")`) but need NO running server, so unlike
//! `run_wait_e2e.rs` they are cheap and run in the default pool. They pin
//! the contracts an agent or shell script depends on:
//!
//!   * A one-shot verb prints NO build banner — stderr stays clean.
//!   * A `--json` path puts ONLY JSON on stdout (or nothing, on error),
//!     never the banner; errors go to stderr with a nonzero exit.
//!   * `--version` reports on stdout, banner-free.
//!   * A verb whose reader hangs up (`| head`, quitting `less`) exits 0 in
//!     silence instead of panicking — see `run_with_closed_stdout`.
//!
//! Every verb that contacts a server is pointed at a socket path that
//! does not exist, so the server is never auto-spawned: `ls` does not
//! auto-start one, and the selector verbs see a connect error first.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::process::{Command, Stdio};

use tempfile::TempDir;

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// A socket path guaranteed not to exist, so no verb finds (or spawns) a
/// server. Unique per process to avoid any cross-run collision.
fn dead_socket() -> String {
    format!("/tmp/phux-no-such-server-{}.sock", std::process::id())
}

/// Run `phux <args...>` and return `(exit_code, stdout, stderr)`.
fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        strip_dhat(&String::from_utf8_lossy(&out.stderr)),
    )
}

fn run_with_xdg(args: &[&str], xdg_config_home: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        strip_dhat(&String::from_utf8_lossy(&out.stderr)),
    )
}

/// Drop `dhat:` diagnostic lines from stderr.
///
/// An `--all-features` build (the `just ci` profile) carries the
/// `dhat-heap` profiler, whose Drop prints nondeterministic heap stats to
/// stderr on every clean `main` return. Those lines are a build diagnostic,
/// not CLI output; left in, they break equality assertions (the stats vary
/// run to run) under that one feature set. Deliberately NOT applied to
/// `run_with_closed_stdout`: the hang-up path must skip destructors
/// entirely (`process::exit`), so dhat output there is a real regression
/// the strict emptiness assertion exists to catch.
fn strip_dhat(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.starts_with("dhat: "))
        .fold(String::new(), |mut acc, line| {
            acc.push_str(line);
            acc.push('\n');
            acc
        })
}

/// The pre-alpha build banner that used to print on EVERY invocation. No
/// one-shot verb may emit it (it pollutes stderr for scripts/agents).
const BANNER_FRAGMENT: &str = "pre-alpha";

#[test]
fn version_is_clean_stdout_with_no_banner() {
    let (code, stdout, stderr) = run(&["--version"]);
    assert_eq!(code, 0, "--version should exit 0; stderr={stderr}");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "--version stdout should carry the version; got {stdout:?}"
    );
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "--version must not print the pre-alpha banner; stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn help_does_not_print_banner() {
    let (code, stdout, stderr) = run(&["--help"]);
    assert_eq!(code, 0, "--help should exit 0");
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "--help must not print the pre-alpha banner; stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn ls_json_no_server_is_silent_stdout_and_banner_free() {
    let sock = dead_socket();
    let (code, stdout, stderr) = run(&["ls", "--json", "--socket", &sock]);
    assert_ne!(code, 0, "`ls --json` with no server should exit nonzero");
    assert!(
        stdout.is_empty(),
        "`ls --json` with no server must leave stdout empty (no banner, no partial JSON); got {stdout:?}"
    );
    assert!(
        !stderr.contains(BANNER_FRAGMENT),
        "`ls --json` must not print the banner to stderr; got {stderr:?}"
    );
    assert!(
        stderr.contains("no server"),
        "the error should explain there is no server; got {stderr:?}"
    );
}

#[test]
fn ls_plain_no_server_is_banner_free() {
    let sock = dead_socket();
    let (code, _stdout, stderr) = run(&["ls", "--socket", &sock]);
    assert_ne!(
        code, 0,
        "`ls` with no server should exit nonzero (like tmux)"
    );
    assert!(
        !stderr.contains(BANNER_FRAGMENT),
        "`ls` must not print the banner; got {stderr:?}"
    );
}

#[test]
fn snapshot_json_no_server_is_silent_stdout_and_banner_free() {
    let sock = dead_socket();
    let (code, stdout, stderr) = run(&["snapshot", "--json", "work", "--socket", &sock]);
    assert_ne!(
        code, 0,
        "`snapshot --json` with no server should exit nonzero"
    );
    assert!(
        stdout.is_empty(),
        "`snapshot --json` with no server must leave stdout empty; got {stdout:?}"
    );
    assert!(
        !stderr.contains(BANNER_FRAGMENT),
        "`snapshot --json` must not print the banner; got {stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// One grammar: the global `--socket` (ADR-0065)
// ---------------------------------------------------------------------------

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
fn config_path_is_clean_stdout_with_no_banner() {
    // `config` never contacts a server, so it always runs. Its stdout must
    // be the path alone — no banner above it.
    let (code, stdout, stderr) = run(&["config", "path"]);
    assert_eq!(code, 0, "`config path` should exit 0; stderr={stderr}");
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "`config path` must not print the banner; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.lines().count() == 1 && !stdout.trim().is_empty(),
        "`config path` stdout should be exactly the path on one line; got {stdout:?}"
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

/// `phux rec --json` against a dead socket: the connect error is the only
/// output, and it goes to stderr. An agent that pipes stdout into a JSON
/// parser must get either one object or nothing — never a banner, never a
/// half-written result line.
#[test]
fn rec_json_no_server_is_silent_stdout_and_banner_free() {
    let tmp = TempDir::new().expect("tempdir");
    let out = tmp.path().join("demo.cast");
    let sock = dead_socket();
    let (code, stdout, stderr) = run(&[
        "rec",
        "--json",
        "-o",
        out.to_str().expect("utf-8 temp path"),
        "--socket",
        &sock,
    ]);

    assert_ne!(code, 0, "`rec --json` with no server should exit nonzero");
    assert!(
        stdout.is_empty(),
        "`rec --json` with no server must leave stdout empty (no banner, no partial JSON); got {stdout:?}"
    );
    assert!(
        !stderr.contains(BANNER_FRAGMENT),
        "`rec --json` must not print the banner to stderr; got {stderr:?}"
    );
    assert!(
        stderr.contains("no server"),
        "the error should explain there is no server; got {stderr:?}"
    );
    assert!(
        !out.exists(),
        "a capture that never started must not leave an empty cast behind"
    );
}

// ---------------------------------------------------------------------------
// The JSON error contract (ADR-0065 §4, phux-i0e8.8.2)
//
// Every converted `--json` verb failing against a dead socket must leave
// stdout empty and put ONE JSON line on stderr:
// {"schema_version":1,"error":{"code":"no_server","message":...},
//  "remedy":...,"exit_code":1} — with the process exiting 1. The prose
// paths (no `--json`) are pinned separately above and in unit tests.
// ---------------------------------------------------------------------------

/// Run `phux <args...>` with an optional `XDG_CONFIG_HOME`, returning
/// `(exit_code, stdout, stderr)`.
fn run_maybe_xdg(args: &[&str], xdg: Option<&std::path::Path>) -> (i32, String, String) {
    xdg.map_or_else(|| run(args), |xdg| run_with_xdg(args, xdg))
}

/// Assert the whole per-verb contract: exit 1, empty stdout, and stderr
/// that is ONE line parsing as the contract document with code `no_server`,
/// `exit_code` 1, the socket named in the message, and a non-empty remedy.
fn assert_no_server_json_contract(
    verb: &str,
    args: &[&str],
    xdg: Option<&std::path::Path>,
    sock: &str,
) {
    let (code, stdout, stderr) = run_maybe_xdg(args, xdg);
    assert_eq!(
        code,
        1,
        "`phux {}` with no server must exit 1; stderr={stderr}",
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
    assert_eq!(doc["error"]["code"], "no_server", "{verb}: {doc}");
    assert!(
        doc["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(sock)),
        "{verb}: the message names the socket; got {doc}"
    );
    assert_eq!(doc["exit_code"], 1, "{verb}: {doc}");
    assert!(
        doc["remedy"].as_str().is_some_and(|r| !r.is_empty()),
        "{verb}: the error must carry a non-empty remedy; got {doc}"
    );
}

/// One converted verb, `--json`, against a dead socket: exit 1, empty
/// stdout, and stderr that parses as the contract document with code
/// `no_server`, `exit_code` 1, and a non-empty remedy.
///
/// The "dead socket" is a real socket file whose listener has gone (bound,
/// then dropped) rather than a missing path, for two reasons: connecting to
/// it is a clean `ECONNREFUSED` on every platform (a plain file would be
/// `ENOTSOCK`), and the existing path keeps `new --json` — which auto-spawns
/// a server when the socket does not exist — on the same connect-refused
/// path as every other verb.
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
            "new",
            vec!["new", "--json", "-s", "x", "--socket", sock],
            None,
        ),
        (
            "ask",
            vec!["ask", "work", "--json", "--socket", sock, "hello?"],
            None,
        ),
    ];

    for (verb, args, xdg) in cases {
        assert_no_server_json_contract(verb, args, *xdg, sock);
    }
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
}

// ---------------------------------------------------------------------------
// Broken pipe (phux-h5hj.8, phux-ngq2)
//
// `phux snapshot work | head -8` used to die with a ~50-frame backtrace and
// an ERROR line reading `server panic`, because `println!` panics when its
// write fails and the panic hook of the day was the SERVER's. Piping a verb
// into `head` or `less` is an ordinary thing for a human to do; it has to
// exit 0 in silence.
// ---------------------------------------------------------------------------

/// Run `phux <args...>` with **the read end of its stdout pipe already
/// closed**, and return `(exit_status_code, stderr)`.
///
/// This closes a real pipe on a real spawned binary. Simulating it — calling
/// the print helper with a fake writer that returns `BrokenPipe` — would
/// prove nothing about the two things that actually broke: the panic hook a
/// dying process runs, and the exit status the shell sees.
///
/// `code` is `None` when the child died from a signal, which is a failure
/// mode this contract must exclude too: Rust masks `SIGPIPE` at startup, and
/// a `phux` that started letting it through would die with 141 instead of
/// reporting 0.
fn run_with_closed_stdout(args: &[&str]) -> (Option<i32>, String) {
    let mut child = Command::new(PHUX)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phux binary");

    // Drop OUR end of the pipe — the only reader — before the child gets
    // through clap parsing and reaches its first write. With no reader left,
    // the write fails with EPIPE immediately, whatever its size; we do not
    // depend on filling a 64 KiB pipe buffer to provoke it.
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

/// `phux completion bash` is the load-bearing case: the script is ~160 KB,
/// so it cannot fit in a pipe buffer and the write is guaranteed to reach a
/// reader that has gone — no scheduling luck involved. It is also the verb
/// whose stdout belongs to `clap_complete`, which `.expect()`s its writes;
/// the fix had to render into a buffer rather than hand it `io::stdout()`.
#[test]
fn completion_survives_a_closed_stdout() {
    assert_survives_closed_stdout(&["completion", "bash"]);
}

/// The `print!`-shaped path (no trailing newline): `config show --default`
/// streams the packaged config as one fragment, so it exercises `out!`
/// rather than `outln!`.
#[test]
fn config_show_default_survives_a_closed_stdout() {
    assert_survives_closed_stdout(&["config", "show", "--default"]);
}

/// A `--json` path. An agent piping `phux config plugins --json` into `jq
/// -e '.plugins[0]'` gets a reader that exits early on the first match; the
/// verb must not turn that into a crash.
#[test]
fn config_plugins_json_survives_a_closed_stdout() {
    assert_survives_closed_stdout(&["config", "plugins", "--json"]);
}

/// A verb that talks to the socket first and reports afterwards. Pointed at
/// a dead socket so no server is spawned — `doctor` reports the absence
/// rather than failing, so it still reaches its writes.
#[test]
fn doctor_survives_a_closed_stdout() {
    let sock = dead_socket();
    assert_survives_closed_stdout(&["doctor", "--socket", &sock]);
}

/// `--version` and `--help` are written by clap, not by us. They are in the
/// same contract — a user pipes `phux --help | head` more often than
/// anything else here — so pin them rather than assume clap keeps handling
/// it.
#[test]
fn clap_output_survives_a_closed_stdout() {
    assert_survives_closed_stdout(&["--version"]);
    assert_survives_closed_stdout(&["--help"]);
}
