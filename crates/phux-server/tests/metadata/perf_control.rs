//! `PERF_CONTROL` over the wire: a trace started and stopped around some
//! commands lands as Chrome trace-event JSON under the state directory, and
//! the heap-accounting switch answers with its state.

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, PerfOp,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{command, join_after_shutdown, run_local, spawn_server_with_seed_cmd};

use crate::common::{connect_as, full_caps};
use crate::get_perf::get_perf;

async fn perf_control(stream: &mut UnixStream, request_id: u32, op: PerfOp) -> CommandResult {
    command(stream, request_id, Command::PerfControl { op }).await
}

fn json(result: CommandResult) -> serde_json::Value {
    let CommandResult::OkWith(CommandValue::Json(json)) = result else {
        panic!("PERF_CONTROL must answer OkWith(Json): {result:?}");
    };
    serde_json::from_str(&json).expect("reply JSON parses")
}

fn refused(result: &CommandResult, needle: &str) {
    match result {
        CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message,
        } => assert!(message.contains(needle), "{message}"),
        other => panic!("expected an INVALID_COMMAND refusal naming {needle:?}: {other:?}"),
    }
}

#[test]
fn perf_control_writes_a_trace_and_switches_heap_accounting() {
    let state = TempDir::new().unwrap();
    // SAFETY: nextest runs each test in its own process, and this runs before
    // any server thread exists; the trace must land in scratch state.
    unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cmd = portable_pty::CommandBuilder::new("/bin/sh");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "perf-control", cmd);

        let (mut stream, hello) = connect_as(&socket_path, "perf-control", full_caps()).await;
        let FrameKind::HelloOk { server_caps, .. } = hello else {
            panic!("expected HELLO_OK: {hello:?}");
        };
        assert!(
            server_caps
                .features_ext
                .contains(ServerFeatureExt::PerfControl),
            "the server advertises PERF_CONTROL"
        );

        refused(
            &perf_control(&mut stream, 1, PerfOp::TraceStop).await,
            "no trace is recording",
        );
        assert_eq!(
            json(perf_control(&mut stream, 2, PerfOp::TraceStart).await)["tracing"],
            true
        );
        refused(
            &perf_control(&mut stream, 3, PerfOp::TraceStart).await,
            "already recording",
        );
        // Timed as `cmd.handle` while the trace is armed.
        get_perf(&mut stream, 4, false).await;
        let reply = json(perf_control(&mut stream, 5, PerfOp::TraceStop).await);

        let path = std::path::PathBuf::from(reply["path"].as_str().expect("path"));
        assert!(
            path.starts_with(state.path()),
            "the trace lands under the state directory: {}",
            path.display()
        );
        assert!(reply["events"].as_u64().expect("events") > 0, "{reply}");
        assert_eq!(reply["dropped"], 0);
        assert!(reply["duration_ms"].as_u64().is_some());
        let trace: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("trace file exists"))
                .expect("trace is JSON");
        let events = trace["traceEvents"].as_array().expect("traceEvents");
        assert!(
            events
                .iter()
                .any(|event| event["ph"] == "X" && event["name"] == "cmd.handle"),
            "a cmd.handle span was captured"
        );

        let on = json(perf_control(&mut stream, 6, PerfOp::AllocOn).await);
        assert!(on["installed"].is_boolean(), "{on}");
        let off = json(perf_control(&mut stream, 7, PerfOp::AllocOff).await);
        assert_eq!(off["alloc"], false);
        refused(
            &perf_control(&mut stream, 8, PerfOp::Unknown(0xEE)).await,
            "unknown op",
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
