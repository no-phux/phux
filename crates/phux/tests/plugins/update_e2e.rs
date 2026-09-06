//! Never-silent-failure coverage for `phux update`, against the real binary.
//!
//! `phux update` replaces the binary the user is running, so its refusals are
//! the product. Every one of them has to be loud, machine-readable under
//! `--json`, and — crucially — has to happen *before* anything is downloaded
//! or written. That last property is what makes these tests possible without
//! a network: each scenario here is a path that refuses on evidence phux
//! already has (the resolved path of its own executable, the shape of a tag,
//! clap's own grammar), so the whole file runs offline in the default pool.
//!
//! Nothing here performs a real download. The download / checksum / atomic
//! replacement / rollback paths are exercised in the crate's unit tests
//! against an injected fake release source
//! (`commands::update::tests`), which is the seam that exists precisely so
//! CI never has to reach github.com to prove the install path works.
//!
//! The binary under test lives in `target/{debug,release}`, which is
//! deliberately NOT a recognized install location — so it is also the
//! fixture for the "unknown source" arm, the one that must never degrade
//! into a best-effort overwrite.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::process::Command;

/// The freshly built binary under test, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Run `phux <args...>` and return `(exit_code, stdout, stderr)`.
fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(PHUX)
        // The allowlist is env-extensible on purpose; a developer who has
        // this set must not turn `target/debug` into a "recognized" install
        // and quietly disarm the unknown-source scenarios below.
        .env_remove("PHUX_INSTALL_DIR")
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Parse the single JSON line the `--json` error contract puts on stderr.
fn json_error(stderr: &str) -> serde_json::Value {
    let line = stderr
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON error line in stderr:\n{stderr}"));
    serde_json::from_str(line)
        .unwrap_or_else(|err| panic!("stderr line is not JSON ({err}):\n{line}"))
}

/// The binary in `target/` is in no recognized install location, so a real
/// update refuses outright. It must not fall back to overwriting itself, and
/// it must not reach the network to find that out.
#[test]
fn an_unrecognized_install_location_is_refused_not_overwritten() {
    let (code, stdout, stderr) = run(&["update"]);
    assert_eq!(code, 2, "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must stay empty: {stdout}");
    assert!(
        stderr.contains("not a recognized phux install location"),
        "the refusal must say why:\n{stderr}"
    );
    assert!(
        stderr.contains(".local/bin"),
        "the refusal must name where phux does maintain installs:\n{stderr}"
    );
}

/// The same refusal, as the machine contract: one JSON object on stderr,
/// stdout empty, the closed-vocabulary code, and the exit code embedded.
#[test]
fn the_unknown_source_refusal_follows_the_json_error_contract() {
    let (code, stdout, stderr) = run(&["update", "--json"]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty(), "stdout must stay empty: {stdout}");
    let doc = json_error(&stderr);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["error"]["code"], "update_source_unsupported");
    assert_eq!(doc["exit_code"], 2);
    assert!(
        doc["remedy"].as_str().is_some_and(|r| !r.is_empty()),
        "a refusal with no remedy is a dead end: {doc}"
    );
}

/// `--rollback` is refused by the same source check, before it goes looking
/// for a backup directory to move files out of.
#[test]
fn rollback_is_refused_on_an_install_phux_does_not_own() {
    let (code, stdout, stderr) = run(&["update", "--rollback", "--json"]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(
        json_error(&stderr)["error"]["code"],
        "update_source_unsupported"
    );
}

/// A bad `--version` is diagnosed offline: tag validation runs before the
/// release index is consulted, so a typo costs one message and no round trip.
#[test]
fn a_tag_that_is_not_a_release_tag_is_refused_without_network() {
    for bad in ["nightly", "1.2.3", "v1.2.3/../evil", "latest"] {
        let (code, stdout, stderr) = run(&["update", "--check", "--version", bad, "--json"]);
        assert_eq!(
            code, 2,
            "`{bad}` should be a usage error; stderr:\n{stderr}"
        );
        assert!(stdout.is_empty(), "stdout must stay empty for `{bad}`");
        let doc = json_error(&stderr);
        assert_eq!(doc["error"]["code"], "update_invalid_tag", "for `{bad}`");
        assert!(
            doc["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains(bad)),
            "the message must quote what was rejected: {doc}"
        );
    }
}

/// `--check` reports; `--dry-run` and `--rollback` act. Combining them is a
/// usage error caught by clap rather than a silently-ignored flag.
#[test]
fn contradictory_flag_combinations_are_usage_errors() {
    for args in [
        ["update", "--check", "--dry-run"],
        ["update", "--check", "--rollback"],
        ["update", "--rollback", "--dry-run"],
    ] {
        let (code, stdout, stderr) = run(&args);
        assert_eq!(code, 2, "{args:?} should be refused; stderr:\n{stderr}");
        assert!(stdout.is_empty(), "{args:?} wrote to stdout: {stdout}");
        assert!(
            stderr.contains("cannot be used with"),
            "{args:?} must explain the conflict:\n{stderr}"
        );
    }
    // `--version` names a release to install, so it cannot ride a rollback.
    let (code, _, stderr) = run(&["update", "--rollback", "--version", "v1.2.3"]);
    assert_eq!(code, 2, "stderr:\n{stderr}");
}

/// `phux upgrade` stays the low-level primitive and keeps its own contract:
/// with no server it reports and exits 1, and it never claims to have
/// fetched anything.
#[test]
fn upgrade_remains_the_local_re_exec_primitive() {
    let socket = format!("/tmp/phux-no-such-server-{}.sock", std::process::id());
    let (code, stdout, stderr) = run(&["--socket", &socket, "upgrade"]);
    assert_eq!(code, 1, "stderr:\n{stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("no server running"),
        "upgrade must still name the missing server:\n{stderr}"
    );
}

/// Both verbs are in the help tree and each points at the other's job, so a
/// user who reaches for the wrong one is told which one they wanted.
#[test]
fn help_distinguishes_update_from_upgrade() {
    let (code, stdout, _) = run(&["update", "--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("verifies it against the checksum"));
    assert!(stdout.contains("--rollback"));

    let (code, stdout, _) = run(&["upgrade", "--help"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("downloads nothing"),
        "`upgrade --help` must say it does not fetch:\n{stdout}"
    );
    assert!(
        stdout.contains("phux update"),
        "`upgrade --help` must point at the verb that does:\n{stdout}"
    );
}
