//! `phux update` refusals against the real binary, offline: each must be
//! loud, machine-readable under `--json`, and happen before anything is
//! downloaded or written. The binary under `target/` is not a recognized
//! install location, so it is the fixture for the unknown-source refusal.
//! Download/checksum/rollback are unit-tested against a fake release source.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use crate::common::Out;

/// Run `phux <args...>` and return `(exit_code, stdout, stderr)`.
fn run(args: &[&str]) -> (i32, String, String) {
    Out::of(
        crate::common::phux_cmd(crate::runner::phux_bin())
            // A developer's allowlist must not make `target/` a recognized install.
            .env_remove("PHUX_INSTALL_DIR")
            .args(args),
    )
    .into_parts()
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

/// The same refusal as the JSON contract, also for `--rollback` (before it
/// looks for a backup) and `--channel next`.
#[test]
fn the_unknown_source_refusal_follows_the_json_error_contract() {
    for args in [
        &["update", "--json"][..],
        &["update", "--rollback", "--json"][..],
        &["update", "--channel", "next", "--json"][..],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        let doc = json_error(&stderr);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["error"]["code"], "update_source_unsupported");
        assert_eq!(doc["exit_code"], 2);
        assert!(
            doc["remedy"].as_str().is_some_and(|r| !r.is_empty()),
            "{doc}"
        );
    }
}

/// A bad `--version` is diagnosed offline: tag validation runs before the
/// release manifest is consulted, so a typo costs one message and no round trip.
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
