//! The Cargo target runner (`scripts/test-env-runner.sh`) stands between
//! every test process and the shell that started it. A test run from inside
//! a production phux pane must not inherit the variables that address the
//! live server, whichever driver started it.

#![cfg(unix)]
#![allow(clippy::expect_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `.cargo/config.toml` [env] sets this for everything Cargo starts. Only the
/// runner removes it, so seeing it here means the runner did not run.
const CANARY: &str = "PHUX_SCRUB_CANARY";

fn runner() -> PathBuf {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    Path::new(&manifest).join("../../scripts/test-env-runner.sh")
}

/// A `deps/print-env` script standing in for a test binary, run through the
/// runner with a hostile environment.
fn run_as_test_binary(dir: &Path, leaf: &str, env: &[(&str, &str)]) -> Output {
    let parent = dir.join(leaf);
    std::fs::create_dir_all(&parent).expect("binary directory");
    let binary = parent.join("print-env");
    std::fs::write(&binary, "#!/bin/sh\nexec /usr/bin/env\n").expect("stub");
    make_executable(&binary);
    let mut cmd = Command::new("bash");
    cmd.arg(runner()).arg(&binary);
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run the runner")
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut mode = std::fs::metadata(path)
        .expect("stub metadata")
        .permissions();
    mode.set_mode(0o755);
    std::fs::set_permissions(path, mode).expect("chmod stub");
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn this_test_ran_under_the_runner() {
    assert_eq!(
        std::env::var_os(CANARY),
        None,
        "the test runner did not run: an inherited PHUX_SOCKET would reach this \
         process. Check [target.'cfg(unix)'] in .cargo/config.toml"
    );
}

#[test]
fn nothing_inherited_addresses_a_server() {
    for key in [
        "PHUX_SOCKET",
        "PHUX_PROFILE",
        "PHUX_TERMINAL_ID",
        "PHUX_WS_TOKENS",
        "PHUX_WS_TLS_CERT",
        "PHUX_WS_TLS_KEY",
        "PHUX_BIN",
    ] {
        assert_eq!(std::env::var_os(key), None, "{key} reached a test process");
    }
}

#[test]
fn a_test_binary_loses_production_addresses_and_keeps_test_knobs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = stdout(&run_as_test_binary(
        dir.path(),
        "deps",
        &[
            ("PHUX_SOCKET", "/tmp/phux-prod/phux.sock"),
            ("PHUX_WS_TOKENS", "secret"),
            ("PHUX_SOMETHING_ADDED_LATER", "1"),
            ("PHUX_DESKTOP_DEV_SOCKET", "/tmp/phux-prod/phux.sock"),
            ("PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE", "600"),
            ("PHUX_TEST_MCP_BIN", "/kept"),
            ("MY_PHUX_THING", "kept"),
        ],
    ));
    for gone in [
        "PHUX_SOCKET=",
        "PHUX_WS_TOKENS=",
        "PHUX_SOMETHING_ADDED_LATER=",
        "PHUX_DESKTOP_DEV_SOCKET=",
    ] {
        assert!(!env.contains(gone), "{gone} survived:\n{env}");
    }
    for kept in [
        "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE=600",
        "PHUX_TEST_MCP_BIN=/kept",
        "MY_PHUX_THING=kept",
    ] {
        assert!(env.contains(kept), "{kept} was removed:\n{env}");
    }
}

#[test]
fn cargo_run_keeps_its_environment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = stdout(&run_as_test_binary(
        dir.path(),
        "debug",
        &[("PHUX_PROFILE", "wt7")],
    ));
    assert!(env.contains("PHUX_PROFILE=wt7"), "{env}");
}

#[test]
fn a_value_with_a_newline_is_not_a_variable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = stdout(&run_as_test_binary(
        dir.path(),
        "deps",
        &[("OTHER", "x\nPHUX_FAKE=1"), ("PHUX_LOG", "a\nb")],
    ));
    assert!(env.contains("OTHER=x\nPHUX_FAKE=1"), "{env}");
    assert!(!env.contains("PHUX_LOG="), "{env}");
}

#[test]
fn the_all_flag_scrubs_any_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = Command::new("bash")
        .arg(runner())
        .args(["--all", "/usr/bin/env"])
        .env("PHUX_SOCKET", "/tmp/phux-prod/phux.sock")
        .env("PHUX_NO_UPDATE_CHECK", "1")
        .current_dir(dir.path())
        .output()
        .expect("run the runner");
    let env = stdout(&output);
    assert!(!env.contains("PHUX_SOCKET="), "{env}");
    assert!(env.contains("PHUX_NO_UPDATE_CHECK=1"), "{env}");
}
