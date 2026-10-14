#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "subprocess test setup must fail loudly"
)]

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

fn mcp() -> Command {
    let mut cmd = Command::new(phux_mcp_bin());
    // Same service-manager exports a live pane inherits (phux-lru0).
    for key in [
        "PHUX_SOCKET",
        "PHUX_WS_TOKENS",
        "PHUX_WS_TLS_CERT",
        "PHUX_WS_TLS_KEY",
        "PHUX_SERVICE_MANAGED",
    ] {
        cmd.env_remove(key);
    }
    cmd
}

fn run(args: &[&str]) -> std::process::Output {
    mcp().args(args).output().expect("run phux-mcp")
}

#[test]
fn skill_is_the_compiled_source_and_help_discovers_it() {
    let output = run(&["--skill"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        include_bytes!("../../../.agents/skills/using-phux-mcp/SKILL.md")
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(concat!(
        "  version: \"",
        env!("CARGO_PKG_VERSION"),
        "\" # x-release-please-version"
    )));

    let help = run(&["--help"]);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--skill"));
    assert!(String::from_utf8_lossy(&help.stdout).contains("--schema"));
}

#[test]
fn schema_is_exactly_the_live_tools_list_catalog() {
    let schema = run(&["--schema"]);
    assert!(schema.status.success());
    assert!(schema.stderr.is_empty());
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();

    let mut child = mcp()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn MCP server");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
        .unwrap();
    let served = child.wait_with_output().unwrap();
    assert!(served.status.success());
    assert!(served.stderr.is_empty());
    let response: Value = serde_json::from_slice(&served.stdout).unwrap();
    assert_eq!(schema, response["result"]["tools"]);
}

#[test]
fn standalone_modes_reject_ambiguity_and_unknown_arguments() {
    for args in [&["--skill", "--schema"][..], &["unknown"][..]] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--skill"));
    }

    let version = run(&["--version"]);
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        concat!("phux-mcp ", env!("CARGO_PKG_VERSION"), "\n")
    );
}

#[test]
fn out_of_band_diagnostics_use_plain_labels_even_when_color_is_forced() {
    let output = mcp()
        .arg("unknown")
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .output()
        .expect("run invalid MCP invocation");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("phux: error: mcp: "));
    assert!(stderr.contains("\n  hint: Try `phux-mcp --help` for usage.\n"));
    assert!(!output.stderr.contains(&0x1b));

    let schema = run(&["--schema"]);
    let forced = mcp()
        .arg("--schema")
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .output()
        .expect("run MCP catalog under forced color");
    assert!(forced.status.success());
    assert_eq!(forced.stdout, schema.stdout);
    assert!(forced.stderr.is_empty());
}

/// The `phux-mcp` binary under test, from the runner's environment
/// (`CARGO_BIN_EXE_*` under `cargo test`, `NEXTEST_BIN_EXE_*` under nextest)
/// rather than `env!`, which would key this test to one checkout.
fn phux_mcp_bin() -> std::ffi::OsString {
    ["CARGO_BIN_EXE_phux-mcp", "NEXTEST_BIN_EXE_phux-mcp"]
        .into_iter()
        .find_map(std::env::var_os)
        .expect("run under `cargo test` or `cargo nextest`, which export the phux-mcp binary path")
}
