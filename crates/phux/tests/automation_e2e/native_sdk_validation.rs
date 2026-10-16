//! Native SDK commands refuse legacy peers before emitting the new command tag.

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
use phux_protocol::caps::ServerFeatureExtSet;
use phux_protocol::native_agent::{NativeCommandResult, NativeDisposition};
use phux_protocol::wire::frame::{Command, FrameKind};

#[test]
fn legacy_native_command_is_refused_not_ambiguous_and_never_transmitted() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("phux.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let spec = ScriptSpec::new().server_features_ext(ServerFeatureExtSet::new());
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
    let operation_id = u64::MAX - 1;
    let output = common::phux_cmd(crate::runner::phux_bin())
        .env("XDG_CONFIG_HOME", dir.path())
        .args([
            "agent",
            "native-command",
            "@9",
            "--activation",
            "activation-A",
            "--operation-id",
            &operation_id.to_string(),
            "--intent",
            r#"{"type":"prompt","text":"never send this"}"#,
            "--json",
            "--socket",
        ])
        .arg(&socket)
        .output()
        .unwrap();
    let seen = server.join().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let receipt: NativeCommandResult = serde_json::from_value(document["result"].clone()).unwrap();
    assert_eq!(receipt.operation_id, operation_id);
    assert_eq!(receipt.disposition, NativeDisposition::Refused);
    assert!(!seen.iter().any(|frame| matches!(
        frame,
        FrameKind::Command {
            command: Command::NativeAgent { .. },
            ..
        }
    )));
}
