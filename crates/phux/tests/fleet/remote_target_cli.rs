//! The `--remote` CLI surface (ADR-0093) without a terminal: malformed
//! targets, root flags before a verb, and `--socket`/`--remote` collisions
//! must exit 2 with their own message, never the TTY-preflight error. The
//! resolution ladder lives in `remote_target_e2e.rs`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use tempfile::TempDir;

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
    let mut cmd = crate::common::phux_cmd(crate::runner::phux_bin());
    cmd.env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PHUX_PROFILE", "default")
        // A path that does not exist: any ssh attempt fails the run loudly
        // instead of silently reaching a real host.
        .env("PHUX_SSH", dir.path().join("no-such-ssh"))
        .args(args);
    crate::common::Out::of(&mut cmd).without_dhat().into_parts()
}

/// Each session verb that takes `--remote`, in a spelling that would
/// otherwise run headlessly (so no TTY preflight can intervene).
const SESSION_VERBS: [&[&str]; 5] = [
    &["ls"],
    &["new", "-s", "x", "--json"],
    &["kill", "--yes", "x"],
    &["rename", "a", "b"],
    &["detach", "--yes"],
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

/// An unregistered host the ssh rung cannot pair fails exactly as attach's
/// ladder does, naming both remedies. Under `--json` the ssh rung is skipped
/// and the refusal is the one-line contract document.
#[test]
fn an_unregistered_host_is_refused_with_the_ladder_remedies() {
    let (code, stderr) = run(&["kill", "--yes", "--remote", "me@mini", "x"]);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stderr.contains("not a registered host")
            && stderr.contains("--code")
            && stderr.contains("phux host add"),
        "the refusal must name both remedies; got: {stderr}"
    );

    let (code, stdout, stderr) = run_with_config(None, &["ls", "--remote", "me@mini", "--json"]);
    assert_eq!(code, 1, "stderr={stderr}");
    let doc = sole_json_error(&stdout, &stderr);
    assert_eq!(doc["error"]["code"], "remote_unresolved");
    assert!(
        !stderr.contains("setting it up over ssh failed"),
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
        stderr.contains("ssh://") && stderr.contains("phux host add mini"),
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
    assert!(remedy.contains("phux host add ghost"), "{remedy}");
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
            .is_some_and(|remedy| remedy.contains("phux host add bare")),
        "{doc}"
    );

    let (code, _stdout, stderr) =
        run_with_config(Some(config), &["kill", "--yes", "--remote", "bare", "x"]);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stderr.contains("no certificate pin") && !stderr.contains("phux attach"),
        "the prose refusal names the entry, not attach: {stderr}"
    );
}

/// Malformed, path-traversing, sigil-leading, and URI targets are usage
/// errors (exit 2), never the TTY refusal. If the TTY message ever shows,
/// target validation has drifted back behind the preflight. A URI is a real
/// thing an operator will try, so its refusal names the verb that takes one.
#[test]
fn a_malformed_target_is_a_usage_error_not_a_tty_error() {
    let malformed = ["", "  ", "@mini", "me@", "mini:0", "mini:70000", "mini:ssh"];
    // These would escape the token directory or shadow the selector grammar.
    let escaping = ["../evil", "#tag", "me@../evil"];
    let cases = malformed
        .iter()
        .chain(&escaping)
        .map(|target| (*target, None))
        .chain([("quic://mini:8788", Some("phux host add"))]);
    for (target, needle) in cases {
        let (code, stderr) = run(&["--remote", target]);
        assert_eq!(code, 2, "target {target:?} must exit 2; stderr={stderr}");
        assert!(
            !stderr.contains("requires both stdin and stdout to be terminals"),
            "target {target:?}: a usage error must precede the TTY preflight; got: {stderr}"
        );
        if let Some(needle) = needle {
            assert!(stderr.contains(needle), "target {target:?}: got: {stderr}");
        }
    }
}

/// Flag combinations the CLI refuses with exit 2, each naming every needle.
const FLAG_REFUSALS: &[(&[&str], &[&str])] = &[
    // The server refuses SHUTDOWN from a remote connection, so this is
    // refused up front with the reason, not after a dial.
    (
        &["kill", "--server", "--remote", "mini"],
        &["local-socket only"],
    ),
    // The root copy belongs to the naked attach, exactly like the root
    // `--rec` (ADR-0065). Silently ignoring it in front of a verb would be
    // worse than either accepting or refusing it.
    (
        &["--remote", "mini", "ls"],
        &["naked `phux` attach", "phux attach --remote"],
    ),
    // `--socket` is a local UDS path and `--remote` is a network dial. clap
    // cannot express this conflict (the root global never meets the
    // sub-matched flag), so every spelling gets an explicit runtime refusal.
    (
        &["--socket", "/tmp/x.sock", "--remote", "mini"],
        &["--socket", "--remote"],
    ),
    (
        &["--remote", "mini", "--socket", "/tmp/x.sock"],
        &["--socket", "--remote"],
    ),
    (
        &["attach", "--remote", "mini", "--socket", "/tmp/x.sock"],
        &["--socket", "--remote"],
    ),
    (
        &["--socket", "/tmp/x.sock", "attach", "--remote", "mini"],
        &["--socket", "--remote"],
    ),
    // `--code` and `--no-enroll` modify `--remote` and mean nothing without it.
    (
        &[
            "attach",
            "--code",
            "https://phux.sh/connect?url=wss://x&token=t",
        ],
        &["--remote"],
    ),
    (&["attach", "--no-enroll"], &["--remote"]),
    // `--remote` picks a host; `--quic`/`--ws` name a raw endpoint. Choosing
    // both is a contradiction clap can catch: all three sit on one verb.
    (
        &["attach", "--remote", "mini", "--quic", "mini:8788"],
        &["--remote"],
    ),
    (
        &["attach", "--remote", "mini", "--ws", "wss://mini:8787"],
        &["--remote"],
    ),
];

#[test]
fn remote_flag_combinations_are_refused_with_their_reason() {
    for (args, needles) in FLAG_REFUSALS {
        let (code, stderr) = run(args);
        assert_eq!(code, 2, "args={args:?} stderr={stderr}");
        for needle in *needles {
            assert!(
                stderr.contains(needle),
                "args={args:?}: missing {needle:?}; got: {stderr}"
            );
        }
    }
}

/// Reading from a machine nobody added refuses it, naming both remedies, and
/// never reaches for ssh: asking what runs there must not set it up.
#[test]
fn reading_an_unregistered_host_never_runs_ssh() {
    let dir = TempDir::new().expect("tempdir");
    let ssh = dir.path().join("ssh");
    let ran = dir.path().join("ssh-ran");
    crate::common::write_executable(
        &ssh,
        format!("#!/bin/sh\ntouch '{}'\nexit 255\n", ran.display()),
    );
    for verb in ["ls", "version", "whoami", "doctor"] {
        let mut cmd = crate::common::phux_cmd(crate::runner::phux_bin());
        cmd.env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_STATE_HOME", dir.path().join("state"))
            .env("PHUX_PROFILE", "default")
            .env("PHUX_SSH", &ssh)
            .args([verb, "--remote", "me@mini"]);
        let (code, _stdout, stderr) = crate::common::Out::of(&mut cmd).without_dhat().into_parts();
        assert_eq!(code, 1, "{verb}: {stderr}");
        assert!(
            stderr.contains("not a registered host")
                && stderr.contains("--code")
                && stderr.contains("phux host add"),
            "{verb}: the refusal must name both remedies; got: {stderr}"
        );
        assert!(!ran.exists(), "{verb} ran ssh: {stderr}");
    }
}

/// A machine ssh gets no answer from is unreachable (exit 4); one that
/// answered and refused the login is a failure (exit 1).
#[test]
fn an_ssh_host_that_never_answers_is_exit_four() {
    for (said, expected) in [
        ("ssh: connect to host mini port 22: Operation timed out", 4),
        ("ssh: connect to host mini port 22: Connection refused", 1),
        ("me@mini: Permission denied (publickey).", 1),
    ] {
        let dir = TempDir::new().expect("tempdir");
        let ssh = dir.path().join("ssh");
        crate::common::write_executable(&ssh, format!("#!/bin/sh\necho '{said}' >&2\nexit 255\n"));
        for args in [
            &["host", "add", "me@mini"][..],
            &["host", "add", "me@mini", "--install", "--json"],
            &["kill", "--yes", "--remote", "me@mini", "x"],
        ] {
            let mut cmd = crate::common::phux_cmd(crate::runner::phux_bin());
            cmd.env("HOME", dir.path())
                .env("XDG_CONFIG_HOME", dir.path().join("config"))
                .env("XDG_STATE_HOME", dir.path().join("state"))
                .env("PHUX_PROFILE", "default")
                .env("PHUX_SSH", &ssh)
                .args(args);
            let (code, _stdout, stderr) =
                crate::common::Out::of(&mut cmd).without_dhat().into_parts();
            assert_eq!(code, expected, "{args:?}: {stderr}");
        }
    }
}
