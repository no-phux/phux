//! The real agent-log CLI must refuse malformed retained output rather than
//! print a successful but incomplete JSON document.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

#[path = "../common/ambient.rs"]
mod common;

use std::os::unix::net::UnixListener;

use phux_client::testkit::{ScriptSpec, ScriptedServer};
use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{Command, FrameKind};
use phux_protocol::wire::info::{
    AgentFacet, ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo,
};

fn snapshot() -> SessionSnapshot {
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    let parent = ResourceId::local(7);
    SessionSnapshot::new(session, window, parent.clone())
        .with_sessions(vec![SessionInfo::new(session, "work")])
        .with_windows(vec![WindowInfo::new(window, session, "shell")])
        .with_resources(vec![
            ResourceInfo::new(parent.clone(), window, 80, 24),
            ResourceInfo::new(ResourceId::local(9), window, 0, 0)
                .with_kind(phux_protocol::ids::ResourceKind::AgentSession)
                .with_parent(Some(parent))
                .with_agent(Some(AgentFacet::new("claude", "working"))),
        ])
}

fn run_log(bytes: &[u8]) -> (std::process::Output, Vec<FrameKind>) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("phux.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let spec = ScriptSpec::new()
        .state(snapshot())
        .server_features(ServerFeatureSet::with(&[ServerFeature::ResourceKinds]))
        .agent_log_bootstrap_bytes(&ResourceId::local(9), bytes, false);
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener).unwrap();
            ScriptedServer::accept(&listener, spec).await
        })
    });
    let output = common::phux_cmd(crate::runner::phux_bin())
        .env("XDG_CONFIG_HOME", dir.path())
        .args(["agent", "log", "@9", "--json", "--socket"])
        .arg(&socket)
        .output()
        .unwrap();
    (output, server.join().unwrap())
}

#[test]
fn malformed_retained_output_fails_loudly_without_a_partial_document() {
    let valid = b"{\"seq\":1,\"ts_ms\":1,\"type\":\"stop\",\"data\":{}}\n";
    let mut partial = valid.to_vec();
    partial.extend_from_slice(b"{\"seq\":2");
    let oversized = format!(
        "{{\"seq\":1,\"ts_ms\":1,\"type\":\"provider_raw\",\"data\":{{\"text\":\"{}\"}}}}\n",
        "x".repeat(phux_client::agent_session::MAX_RECORD_BYTES),
    )
    .into_bytes();
    for payload in [partial, oversized] {
        let (output, seen) = run_log(&payload);
        assert!(
            !output.status.success(),
            "malformed bootstrap must not succeed"
        );
        assert!(
            output.stdout.is_empty(),
            "no successful partial JSON document"
        );
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(
            error.to_string().contains("malformed AgentEventsJsonlV1"),
            "{error}"
        );
        assert!(
            seen.iter().any(|frame| matches!(frame, FrameKind::Command {
            command: Command::DetachResource { terminal_id }, ..
        } if *terminal_id == ResourceId::local(9))),
            "decoder failure must release the observer"
        );
    }
}
