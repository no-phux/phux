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
    let dir = TempDir::new().expect("tempdir");
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
        stderr,
    )
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
    let (code, stderr) = run(&["attach", "--code", "phux://connect?url=wss://x&token=t"]);
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
