//! Run the view-aware JS bindings against an isolated real PTY-backed server.
//! Build phux-client-ffi with --no-default-features --features napi first,
//! copy its cdylib to a .node path, then pass that absolute path as argv[1].

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    reason = "executable test fixture"
)]
#![allow(
    clippy::future_not_send,
    reason = "the real server owns its engine on a LocalSet"
)]

use std::time::Duration;

use phux_client_runtime::control::{ControlOptions, Event, TerminalResizeOutcome};
use phux_client_runtime::{ClientOptions, Runtime, Target};
use phux_protocol::wire::frame::RolePolicy;
use phux_server_testkit::{SERVER_JOIN_DEADLINE, run_local, spawn_server_with};

async fn wait_for<T>(description: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(value) = poll() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timeout: {description}"))
}

// The current JS connect DTO has no role option. Exercise an actual viewer
// through the same runtime/server here; module tests cover the NAPI resize
// helper's Observer mapping with an explicitly viewer-configured Client.
async fn observer_smoke(socket: &std::path::Path) {
    let observer = Runtime::connect(
        Target::uds(socket),
        ClientOptions {
            control: ControlOptions {
                attach: None,
                attach_role: Some(RolePolicy::VIEWER),
                viewport: (5, 2),
                auto_attach_foreign_spawns: false,
                ..ControlOptions::default()
            },
            ..ClientOptions::default()
        },
    )
    .expect("observer connection");
    let before = wait_for("observer topology", || {
        observer.topology().filter(|graph| graph.panes.len() >= 2)
    })
    .await;
    let pane = &before.panes[0];
    assert_ne!(
        observer.attach_terminal_preserving_geometry(&pane.terminal_id),
        0
    );
    let frame = wait_for("observer projection", || {
        observer.acquire(&pane.terminal_id)
    })
    .await;
    assert_eq!((frame.cols, frame.rows), (pane.cols, pane.rows));
    assert_eq!(
        observer.resize_terminal(&pane.terminal_id, 5, 2),
        TerminalResizeOutcome::Observer
    );
    let _ = observer.take_events();
    assert!(observer.refresh_topology().is_some());
    wait_for("authoritative observer readback", || {
        observer
            .take_events()
            .iter()
            .any(|event| matches!(event, Event::TopologyChanged))
            .then_some(())
    })
    .await;
    let after = observer.topology().expect("refreshed topology");
    for pane in before.panes {
        let unchanged = after
            .panes
            .iter()
            .find(|other| other.terminal_id == pane.terminal_id)
            .expect("same PTY");
        assert_eq!((unchanged.cols, unchanged.rows), (pane.cols, pane.rows));
    }
    observer.close();
    println!(
        "Native observer smoke passed: preserving subscription, resize refusal, authoritative two-PTY geometry unchanged"
    );
}

fn main() {
    let addon = std::env::args()
        .nth(1)
        .expect("usage: napi_view_smoke /absolute/path/phux_client_ffi.node");
    run_local(async move {
        let tmp = tempfile::TempDir::new().expect("fixture directory");
        let socket = tmp.path().join("napi-view.sock");
        let (shutdown, server) =
            spawn_server_with(socket.clone(), Some("napi-view-smoke"), |config| {
                config.seed_with_pty = true;
            });
        let script =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/napi-view-smoke.cjs");
        let status = tokio::time::timeout(
            Duration::from_secs(60),
            tokio::process::Command::new("node")
                .arg(script)
                .arg(addon)
                .arg(&socket)
                .kill_on_drop(true)
                .status(),
        )
        .await
        .expect("JS smoke deadline")
        .expect("start node");
        if status.success() {
            observer_smoke(&socket).await;
        }
        let _ = shutdown.send(());
        tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
            .await
            .expect("server stop deadline")
            .expect("server join")
            .expect("server shutdown");
        assert!(status.success(), "NAPI view JS smoke failed: {status}");
    });
}
