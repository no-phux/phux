//! The ADR-0066 deprecation contract, pinned at the binary level.
//!
//! `phux remote`, `phux satellite`, and top-level `phux enroll` are hidden
//! aliases of `phux host` for one release cycle. Each must:
//!
//!   * still parse (full arg surface) and run the `host` implementation;
//!   * print EXACTLY ONE stderr deprecation line naming the visible
//!     replacement — and only on the human path: under `--json`, stdout
//!     carries only the document and a failure is one stderr contract
//!     line, so the note is suppressed on both;
//!   * be absent from `phux --help`, while `phux host` is present.
//!
//! None of these verbs contact a server, so the tests are cheap and run in
//! the default pool (the `output_hygiene.rs` pattern).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Run `phux <args...>` under a scratch `XDG_CONFIG_HOME` and return
/// `(exit_code, stdout, stderr)`, with `dhat:` build diagnostics stripped
/// (the `--all-features` profile prints heap stats on clean exit).
fn run_with_xdg(args: &[&str], xdg_config_home: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .args(args)
        .output()
        .expect("run phux binary");
    let stderr = String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|line| !line.starts_with("dhat: "))
        .fold(String::new(), |mut acc, line| {
            acc.push_str(line);
            acc.push('\n');
            acc
        });
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr,
    )
}

/// Assert `stderr` carries the deprecation note for `new_form` exactly once.
fn assert_one_note(stderr: &str, old_form: &str, new_form: &str) {
    let expected =
        format!("phux: `{old_form}` is deprecated and will be removed; use `{new_form}`");
    let count = stderr.lines().filter(|line| *line == expected).count();
    assert_eq!(
        count, 1,
        "expected exactly one deprecation line {expected:?}; stderr={stderr:?}"
    );
    assert_eq!(
        stderr.matches("deprecated").count(),
        1,
        "exactly one deprecation mention; stderr={stderr:?}"
    );
}

#[test]
fn remote_verbs_run_via_host_with_one_stderr_note() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");

    // add: registers into [[remote]] through the host implementation.
    let (code, stdout, stderr) = run_with_xdg(&["remote", "add", "mini", "ssh://mini"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_one_note(&stderr, "phux remote add", "phux host add");
    assert!(
        stdout.contains("Registered remote \"mini\""),
        "the host implementation reports the registration: {stdout:?}"
    );
    let config = std::fs::read_to_string(xdg.join("phux").join("config.toml")).expect("config");
    assert!(config.contains("[[remote]]"), "got {config}");

    // list: the host table, deprecation note on stderr only.
    let (code, stdout, stderr) = run_with_xdg(&["remote", "list"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_one_note(&stderr, "phux remote list", "phux host ls");
    assert!(
        stdout.contains("mini") && stdout.contains("ROLE"),
        "the host table answers: {stdout:?}"
    );

    // remove: through host rm, role-filtered to the remote registry.
    let (code, stdout, stderr) = run_with_xdg(&["remote", "remove", "mini"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_one_note(&stderr, "phux remote remove", "phux host rm");
    assert!(stdout.contains("Removed remote \"mini\""), "got {stdout:?}");
}

#[test]
fn satellite_verbs_run_via_host_with_one_stderr_note() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");

    let (code, stdout, stderr) = run_with_xdg(&["satellite", "add", "edge", "ssh://edge"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_one_note(
        &stderr,
        "phux satellite add",
        "phux host add --role satellite",
    );
    assert!(
        stdout.contains("Registered satellite \"edge\""),
        "got {stdout:?}"
    );
    let config = std::fs::read_to_string(xdg.join("phux").join("config.toml")).expect("config");
    assert!(config.contains("[[satellites]]"), "got {config}");

    let (code, stdout, stderr) = run_with_xdg(&["satellite", "list"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_one_note(
        &stderr,
        "phux satellite list",
        "phux host ls --role satellite",
    );
    assert!(
        stdout.contains("edge") && stdout.contains("satellite"),
        "the role-filtered host table answers: {stdout:?}"
    );

    let (code, stdout, stderr) = run_with_xdg(&["satellite", "rm", "edge"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_one_note(
        &stderr,
        "phux satellite remove",
        "phux host rm --role satellite",
    );
    assert!(
        stdout.contains("Removed satellite \"edge\""),
        "got {stdout:?}"
    );
}

/// `phux enroll HOST --ssh-only` never leaves the machine, so the whole
/// legacy flow is exercisable: it must register a `[[remote]]` entry via
/// the host implementation behind one stderr note.
#[test]
fn legacy_enroll_ssh_only_runs_via_host_enroll() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");

    let (code, stdout, stderr) = run_with_xdg(&["enroll", "me@mini", "--ssh-only"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_one_note(&stderr, "phux enroll", "phux host enroll");
    assert!(
        stdout.contains("Enrolled remote \"mini\" -> ssh://me@mini"),
        "the host implementation reports the enrollment: {stdout:?}"
    );
    let config = std::fs::read_to_string(xdg.join("phux").join("config.toml")).expect("config");
    assert!(
        config.contains("[[remote]]") && config.contains("ssh://me@mini"),
        "got {config}"
    );
}

/// Under `--json` the alias output is bit-for-bit the host contract: the
/// document on stdout (host schema, not the retired per-verb shapes) and
/// NO deprecation note anywhere — stdout stays parseable, and a failure
/// stays ONE stderr contract line.
#[test]
fn json_paths_suppress_the_note_and_emit_the_host_schema() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");

    let (code, stdout, stderr) =
        run_with_xdg(&["satellite", "add", "edge", "ssh://edge", "--json"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stdout.contains("deprecated") && !stderr.contains("deprecated"),
        "`--json` must suppress the deprecation note; stdout={stdout:?} stderr={stderr:?}"
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is one JSON doc");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["host"]["name"], "edge");
    assert_eq!(doc["host"]["role"], "satellite");

    let (code, stdout, stderr) = run_with_xdg(&["remote", "list", "--json"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stderr.contains("deprecated"), "stderr={stderr:?}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is one JSON doc");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(
        doc["hosts"].as_array().expect("hosts").len(),
        0,
        "role-filtered"
    );

    // A provoked failure under `--json`: one stderr line, still no note.
    let (code, stdout, stderr) = run_with_xdg(&["satellite", "remove", "ghost", "--json"], &xdg);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "failure stdout stays empty: {stdout:?}");
    let line = stderr.trim();
    assert!(
        !line.contains('\n'),
        "a `--json` failure is ONE stderr line: {stderr:?}"
    );
    let doc: serde_json::Value = serde_json::from_str(line).expect("contract line parses");
    assert_eq!(doc["error"]["code"], "registry");
}

/// The visible surface: `phux --help` lists `host` and none of the three
/// legacy verbs, while each hidden alias still answers `--help` with its
/// deprecation pointer for whoever lands on it.
#[test]
fn help_hides_the_aliases_but_they_still_explain_themselves() {
    let tmp = TempDir::new().expect("tempdir");

    let (code, stdout, _) = run_with_xdg(&["--help"], tmp.path());
    assert_eq!(code, 0);
    assert!(stdout.contains("host"), "`host` is the visible verb");
    for verb in ["enroll ", "remote ", "satellite "] {
        assert!(
            !stdout
                .lines()
                .any(|line| line.trim_start().starts_with(verb)),
            "`phux --help` still advertises the hidden alias `{verb}`:\n{stdout}"
        );
    }

    for verb in ["remote", "satellite", "enroll"] {
        let (code, stdout, _) = run_with_xdg(&[verb, "--help"], tmp.path());
        assert_eq!(code, 0, "`phux {verb} --help` must still answer");
        assert!(
            stdout.contains("Deprecated alias") && stdout.contains("phux host"),
            "`phux {verb} --help` must point at the replacement:\n{stdout}"
        );
    }
}
