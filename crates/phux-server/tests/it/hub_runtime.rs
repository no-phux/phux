//! Integration tests for hub-mode server startup (phux-v45.1, ADR-0007).
//!
//! Covers the runtime seam around `crate::hub` (the parsing matrix itself
//! is unit-tested in `phux_server::hub`):
//!
//! * `hub_startup_rejects_malformed_registry` — a hub whose registry has a
//!   malformed enabled endpoint fails `run_async` with `ServerError::Hub`
//!   *before* binding the socket (fail-fast: no socket file appears).
//! * `hub_startup_accepts_valid_registry` — a hub with a valid registry
//!   (including a disabled, malformed entry that must be skipped) starts
//!   and shuts down cleanly.
//! * `non_hub_startup_never_reads_the_registry` — without `.hub(...)` the
//!   server starts fine; the registry is not consulted at all.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_config::SatelliteConfigEntry;
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tempfile::TempDir;

fn entry(name: &str, endpoint: &str, enabled: bool) -> SatelliteConfigEntry {
    SatelliteConfigEntry {
        name: name.to_owned(),
        endpoint: endpoint.to_owned(),
        enabled,
        token_file: None,
        cert_fingerprint: None,
    }
}

fn cfg_at(dir: &TempDir) -> (ServerConfig, std::path::PathBuf) {
    let socket_path = dir.path().join("phux.sock");
    let cfg = ServerConfig {
        socket_path: socket_path.clone(),
        pre_seeded_session: None,
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    (cfg, socket_path)
}

fn current_thread_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime")
}

#[test]
fn hub_startup_rejects_malformed_registry() {
    let dir = TempDir::new().unwrap();
    let (cfg, socket_path) = cfg_at(&dir);

    let server = ServerRuntime::new(cfg).hub(vec![
        entry("devbox", "quic://devbox:8788", true),
        entry("broken", "gopher://nope", true),
    ]);

    let rt = current_thread_rt();
    let result = rt.block_on(server.run_async(async {}));

    match result {
        Err(ServerError::Hub(err)) => {
            let msg = err.to_string();
            assert!(msg.contains("broken"), "error should name the entry: {msg}");
            assert!(
                msg.contains("gopher://nope"),
                "error should quote the endpoint: {msg}"
            );
        }
        other => panic!("expected ServerError::Hub, got {other:?}"),
    }
    // Fail-fast: the registry is validated before any I/O, so the socket
    // must never have been bound.
    assert!(
        !socket_path.exists(),
        "malformed hub registry must fail before the socket is bound"
    );
}

#[test]
fn hub_startup_accepts_valid_registry() {
    let dir = TempDir::new().unwrap();
    let (cfg, _socket_path) = cfg_at(&dir);

    let server = ServerRuntime::new(cfg).hub(vec![
        entry("devbox", "quic://devbox:8788", true),
        entry("web", "wss://web.example:8787", true),
        entry("legacy", "ssh://legacy-host", true),
        // Disabled AND malformed: must be skipped, not validated.
        entry("parked", "definitely not a uri", false),
    ]);

    let rt = current_thread_rt();
    // Immediate shutdown: the server validates the table, binds, observes
    // the already-resolved shutdown future, and exits cleanly.
    rt.block_on(server.run_async(async {}))
        .expect("hub server with a valid registry must start");
}

#[test]
fn non_hub_startup_never_reads_the_registry() {
    let dir = TempDir::new().unwrap();
    let (cfg, _socket_path) = cfg_at(&dir);

    // No `.hub(...)`: the registry — however broken it may be in the
    // user's config.toml — is not consulted. (The unit test
    // `hub::tests::non_hub_mode_ignores_the_registry` proves the gate
    // with garbage entries; this proves the default runtime path.)
    let server = ServerRuntime::new(cfg);

    let rt = current_thread_rt();
    rt.block_on(server.run_async(async {}))
        .expect("non-hub server must start without touching the registry");
}
