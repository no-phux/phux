//! Composition probes must be safe with an unknown installation/environment.
#![cfg(unix)]
#![allow(clippy::unwrap_used, reason = "test fixture assertions")]

use std::os::unix::net::UnixListener;
use std::process::Command;

#[test]
fn runtime_info_does_not_dial_write_logs_or_read_broken_config() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("s");
    let log = temp.path().join("must-not-exist.log");
    let config_dir = temp.path().join("phux");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "[malformed").unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_phux"))
        .args(["runtime-info", "--json"])
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path())
        .env("PHUX_SOCKET", &socket)
        .env("PHUX_LOG", &log)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty());
    let doc: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["binary"], "phux");
    assert_eq!(
        doc["protocol"]["minor"],
        phux_protocol::PROTOCOL_VERSION.minor
    );
    assert!(!log.exists(), "read-only runtime-info created a log");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        std::fs::read_to_string(config_dir.join("config.toml")).unwrap(),
        "[malformed"
    );
}

#[test]
fn runtime_info_refuses_explicit_socket_rather_than_pretend_it_probed_that_server() {
    let result = Command::new(env!("CARGO_BIN_EXE_phux"))
        .args(["--socket", "/unused-fixture.sock", "runtime-info", "--json"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("runtime-info"));
}
