//! The `--remote` CLI surface, pinned at the binary level (ADR-0093).
//!
//! Everything here is reachable WITHOUT a terminal, on purpose. A malformed
//! target, a root flag in front of a verb, and a `--socket`/`--remote`
//! collision are usage errors, and a usage error that only reports itself on
//! a TTY is a usage error a script cannot read. These tests are the pin on
//! that ordering: each must exit 2 with its own message, never with
//! "interactive use requires both stdin and stdout to be terminals".
//!
//! The resolution ladder itself (registry hit, `--code`, ssh pairing) needs
//! a real attach and lives in `remote_target_e2e.rs`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Run `phux <args...>` against a private config/state dir with no stdin,
/// so nothing here can touch the developer's real registry. Returns
/// `(exit_code, stderr)`.
fn run(args: &[&str]) -> (i32, String) {
    let (code, _stdout, stderr) = run_with_config(None, args);
    (code, stderr)
}

/// [`run`] with `config` written as the private `config.toml` first, also
/// returning stdout: `(exit_code, stdout, stderr)`.
fn run_with_config(config: Option<&str>, args: &[&str]) -> (i32, String, String) {
    let dir = TempDir::new().expect("tempdir");
    if let Some(config) = config {
        let config_dir = dir.path().join("config/phux");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        std::fs::write(config_dir.join("config.toml"), config).expect("write config");
    }
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PHUX_PROFILE", "default")
        // A path that does not exist: any ssh attempt fails the run loudly
        // instead of silently reaching a real host.
        .env("PHUX_SSH", dir.path().join("no-such-ssh"))
        .args(args)
        .output()
        .expect("run phux binary");
    let stderr = String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|line| !line.starts_with("dhat: "))
        .collect::<Vec<_>>()
        .join("\n");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr,
    )
}

/// Each session verb that takes `--remote`, in a spelling that would
/// otherwise run headlessly (so no TTY preflight can intervene).
const SESSION_VERBS: [&[&str]; 5] = [
    &["ls"],
    &["new", "-s", "x", "--json"],
    &["kill", "x"],
    &["rename", "a", "b"],
    &["detach"],
];

/// The session verbs share attach's usage rules: a malformed target is exit
/// 2 before any dial, and so is `--socket` in either position.
#[test]
fn session_verbs_share_the_remote_usage_rules() {
    for verb in SESSION_VERBS {
        let mut malformed = verb.to_vec();
        malformed.extend(["--remote", "quic://mini:8788"]);
        let (code, stderr) = run(&malformed);
        assert_eq!(code, 2, "args={malformed:?} stderr={stderr}");
        assert!(
            stderr.contains("phux host add"),
            "args={malformed:?}: {stderr}"
        );

        let mut after = verb.to_vec();
        after.extend(["--remote", "mini", "--socket", "/tmp/x.sock"]);
        let mut before = vec!["--socket", "/tmp/x.sock"];
        before.extend(verb.iter().copied());
        before.extend(["--remote", "mini"]);
        for args in [after, before] {
            let (code, stderr) = run(&args);
            assert_eq!(code, 2, "args={args:?} stderr={stderr}");
            assert!(
                stderr.contains("--socket") && stderr.contains("--remote"),
                "args={args:?}: the refusal must name both flags; got: {stderr}"
            );
        }
    }
}

/// The server refuses SHUTDOWN from a remote connection, so `kill --server
/// --remote` is refused up front with the reason, not after a dial.
#[test]
fn kill_server_refuses_remote_before_dialing() {
    let (code, stderr) = run(&["kill", "--server", "--remote", "mini"]);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(stderr.contains("local-socket only"), "got: {stderr}");
}

/// An unregistered host the ssh rung cannot pair fails exactly as attach's
/// ladder does, naming both remedies. Under `--json` the ssh rung is skipped
/// and the refusal is the one-line contract document.
#[test]
fn an_unregistered_host_is_refused_with_the_ladder_remedies() {
    let (code, stderr) = run(&["ls", "--remote", "me@mini"]);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stderr.contains("not a registered host")
            && stderr.contains("--code")
            && stderr.contains("phux host enroll"),
        "the refusal must name both remedies; got: {stderr}"
    );

    let (code, stdout, stderr) = run_with_config(None, &["ls", "--remote", "me@mini", "--json"]);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stdout.is_empty(),
        "--json failure leaves stdout empty: {stdout}"
    );
    assert_eq!(
        stderr.lines().count(),
        1,
        "one JSON line on stderr: {stderr}"
    );
    let doc: serde_json::Value = serde_json::from_str(&stderr).expect("stderr is JSON");
    assert_eq!(doc["error"]["code"], "remote_unresolved");
    assert!(
        !stderr.contains("pairing over ssh"),
        "--json must not attempt the ssh rung: {stderr}"
    );
}

/// An `ssh://` entry carries an interactive attach only; the session verbs
/// refuse it and name the verb that gives the host a dialable endpoint.
#[test]
fn an_ssh_entry_is_refused_for_session_verbs() {
    let config = "[[remote]]\nname = \"mini\"\nendpoint = \"ssh://mini\"\n";
    let (code, _stdout, stderr) = run_with_config(Some(config), &["ls", "--remote", "mini"]);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stderr.contains("ssh://") && stderr.contains("phux host enroll mini"),
        "got: {stderr}"
    );
}

/// Parse the single JSON line a `--json` failure leaves on stderr, asserting
/// it is the only line and that stdout stayed empty.
fn sole_json_error(stdout: &str, stderr: &str) -> serde_json::Value {
    assert!(
        stdout.is_empty(),
        "--json failure leaves stdout empty: {stdout}"
    );
    assert_eq!(
        stderr.lines().count(),
        1,
        "one JSON line on stderr: {stderr}"
    );
    serde_json::from_str(stderr).expect("stderr is one JSON document")
}

/// A registered host whose name does not resolve is a reachability failure
/// on the `--json` contract: exactly one JSON line, code `transport`, exit 1,
/// and a remedy that names the registry entry rather than attach's flags.
#[test]
fn an_unresolvable_entry_is_one_json_line_under_json() {
    let config = "[[remote]]\nname = \"ghost\"\nendpoint = \"quic://ghost.invalid:8788\"\n";
    let (code, stdout, stderr) =
        run_with_config(Some(config), &["ls", "--remote", "ghost", "--json"]);
    assert_eq!(code, 1, "stderr={stderr}");
    let doc = sole_json_error(&stdout, &stderr);
    assert_eq!(doc["error"]["code"], "transport", "{doc}");
    assert_eq!(doc["exit_code"], 1, "{doc}");
    let remedy = doc["remedy"].as_str().unwrap_or_default();
    assert!(remedy.contains("phux host enroll ghost"), "{remedy}");
    assert!(
        !stderr.contains("phux attach") && !stderr.contains("QUIC attach"),
        "the refusal must not be worded for attach: {stderr}"
    );
}

/// A routable entry with no certificate pin cannot be dialed as registered:
/// one JSON line, code `remote_unresolved`, exit 1. Network-free (an IP
/// literal never touches DNS and the refusal precedes any dial).
#[test]
fn an_unpinned_routable_entry_is_one_json_line_under_json() {
    let config = "[[remote]]\nname = \"bare\"\nendpoint = \"quic://203.0.113.7:8788\"\n";
    let (code, stdout, stderr) =
        run_with_config(Some(config), &["ls", "--remote", "bare", "--json"]);
    assert_eq!(code, 1, "stderr={stderr}");
    let doc = sole_json_error(&stdout, &stderr);
    assert_eq!(doc["error"]["code"], "remote_unresolved", "{doc}");
    assert!(
        doc["remedy"]
            .as_str()
            .is_some_and(|remedy| remedy.contains("phux host enroll bare")),
        "{doc}"
    );

    let (code, _stdout, stderr) = run_with_config(Some(config), &["kill", "--remote", "bare", "x"]);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stderr.contains("no certificate pin") && !stderr.contains("phux attach"),
        "the prose refusal names the entry, not attach: {stderr}"
    );
}

/// `--remote` appears in each session verb's help.
#[test]
fn session_verbs_document_remote() {
    for verb in ["ls", "new", "kill", "rename", "detach"] {
        let out = Command::new(PHUX)
            .args([verb, "--help"])
            .output()
            .expect("run phux <verb> --help");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("--remote"),
            "`phux {verb} --help` must show --remote"
        );
    }
}

/// The TTY refusal is the wrong answer to a usage question. If this ever
/// fires, target validation has drifted back behind the preflight.
fn assert_not_the_tty_error(stderr: &str) {
    assert!(
        !stderr.contains("requires both stdin and stdout to be terminals"),
        "a usage error must be reported before the TTY preflight; got: {stderr}"
    );
}

#[test]
fn a_malformed_target_is_a_usage_error_not_a_tty_error() {
    for target in ["", "  ", "@mini", "me@", "mini:0", "mini:70000", "mini:ssh"] {
        let (code, stderr) = run(&["--remote", target]);
        assert_eq!(code, 2, "target {target:?} must exit 2; stderr={stderr}");
        assert_not_the_tty_error(&stderr);
    }
}

/// A URI is a real thing an operator will try. The refusal names the verb
/// that does take one instead of guessing at the endpoint.
#[test]
fn a_uri_target_is_refused_and_names_host_add() {
    let (code, stderr) = run(&["--remote", "quic://mini:8788"]);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(
        stderr.contains("phux host add"),
        "the refusal must name the verb that accepts a URI; got: {stderr}"
    );
    assert_not_the_tty_error(&stderr);
}

/// A path-traversing or sigil-leading host would escape the token directory
/// or shadow the selector grammar. Both are refused at parse.
#[test]
fn a_target_that_could_escape_or_shadow_is_refused() {
    for target in ["../evil", "#tag", "me@../evil"] {
        let (code, stderr) = run(&["--remote", target]);
        assert_eq!(code, 2, "target {target:?} must exit 2; stderr={stderr}");
        assert_not_the_tty_error(&stderr);
    }
}

/// The root copy belongs to the naked attach, exactly like the root `--rec`
/// (ADR-0065). Silently ignoring it in front of a verb would be worse than
/// either accepting or refusing it.
#[test]
fn a_root_remote_in_front_of_a_verb_is_refused_with_the_remedy() {
    let (code, stderr) = run(&["--remote", "mini", "ls"]);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(
        stderr.contains("naked `phux` attach") && stderr.contains("phux attach --remote"),
        "the refusal must name the verb-scoped spelling; got: {stderr}"
    );
}

/// `--socket` is a local UDS path and `--remote` is a network dial. clap
/// cannot express this conflict (the root global never meets the sub-matched
/// flag), so both spellings get an explicit runtime refusal.
#[test]
fn socket_and_remote_cannot_combine_in_either_position() {
    for args in [
        ["--socket", "/tmp/x.sock", "--remote", "mini"].as_slice(),
        ["--remote", "mini", "--socket", "/tmp/x.sock"].as_slice(),
        ["attach", "--remote", "mini", "--socket", "/tmp/x.sock"].as_slice(),
        ["--socket", "/tmp/x.sock", "attach", "--remote", "mini"].as_slice(),
    ] {
        let (code, stderr) = run(args);
        assert_eq!(code, 2, "args={args:?} stderr={stderr}");
        assert!(
            stderr.contains("--socket") && stderr.contains("--remote"),
            "args={args:?}: the refusal must name both flags; got: {stderr}"
        );
    }
}

/// `--code` and `--no-enroll` modify `--remote` and mean nothing without it.
#[test]
fn code_and_no_enroll_require_remote() {
    let (code, stderr) = run(&[
        "attach",
        "--code",
        "https://phux.phall.io/connect?url=wss://x&token=t",
    ]);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(stderr.contains("--remote"), "got: {stderr}");

    let (code, stderr) = run(&["attach", "--no-enroll"]);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(stderr.contains("--remote"), "got: {stderr}");
}

/// `--remote` picks a host; `--quic`/`--ws` name a raw endpoint. Choosing
/// both is a contradiction clap can catch, because all three sit on one verb.
#[test]
fn remote_conflicts_with_the_raw_transport_flags() {
    for other in [["--quic", "mini:8788"], ["--ws", "wss://mini:8787"]] {
        let (code, stderr) = run(&["attach", "--remote", "mini", other[0], other[1]]);
        assert_eq!(code, 2, "other={other:?} stderr={stderr}");
        assert!(
            stderr.contains("--remote"),
            "other={other:?}: got: {stderr}"
        );
    }
}

/// `--remote` must appear in help where an operator will look for it.
#[test]
fn remote_is_documented_on_both_surfaces() {
    let out = Command::new(PHUX)
        .args(["--help"])
        .output()
        .expect("run phux --help");
    let root = String::from_utf8_lossy(&out.stdout);
    assert!(
        root.contains("--remote"),
        "the root help must show --remote"
    );

    let out = Command::new(PHUX)
        .args(["attach", "--help"])
        .output()
        .expect("run phux attach --help");
    let attach = String::from_utf8_lossy(&out.stdout);
    for flag in ["--remote", "--code", "--no-enroll"] {
        assert!(
            attach.contains(flag),
            "`phux attach --help` must show {flag}"
        );
    }
}
