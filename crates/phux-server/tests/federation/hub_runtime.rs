//! Hub-mode startup validates the `[[satellites]]` registry before binding
//! (the parsing matrix is unit-tested in `phux_server::hub`).

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

/// Run a hub with `registry` that shuts down immediately.
fn run_hub(dir: &TempDir, registry: Vec<SatelliteConfigEntry>) -> Result<(), ServerError> {
    let cfg = ServerConfig {
        socket_path: dir.path().join("phux.sock"),
        pre_seeded_session: None,
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(ServerRuntime::new(cfg).hub(registry).run_async(async {}))
}

#[test]
fn hub_startup_rejects_a_malformed_enabled_entry_before_binding() {
    let dir = TempDir::new().unwrap();
    let result = run_hub(
        &dir,
        vec![
            entry("devbox", "quic://devbox:8788", true),
            entry("broken", "gopher://nope", true),
        ],
    );
    let Err(ServerError::Hub(err)) = result else {
        panic!("expected ServerError::Hub, got {result:?}");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("broken") && msg.contains("gopher://nope"),
        "{msg}"
    );
    assert!(
        !dir.path().join("phux.sock").exists(),
        "fails before binding"
    );
}

#[test]
fn hub_startup_skips_disabled_entries() {
    let dir = TempDir::new().unwrap();
    run_hub(
        &dir,
        vec![
            entry("devbox", "quic://devbox:8788", true),
            entry("web", "wss://web.example:8787", true),
            entry("legacy", "ssh://legacy-host", true),
            entry("parked", "definitely not a uri", false),
        ],
    )
    .expect("a valid registry with a disabled malformed entry starts");
}
