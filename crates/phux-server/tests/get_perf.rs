//! `GET_PERF` over the wire: the command is advertised, answers with a JSON
//! `PerfReport`, and the hot-path metrics move when a pane produces output.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_perf::{MetricValue, PerfReport};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeature};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, TYPE_ATTACH_READY, TYPE_COMMAND_RESULT,
    TYPE_HELLO_OK,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_raw_socket,
};

const SESSION: &str = "perf";

async fn connect(path: &std::path::Path) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "get-perf-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK, "HELLO must be accepted");
    let FrameKind::HelloOk { server_caps, .. } = frame else {
        panic!("expected HELLO_OK, got {frame:?}");
    };
    assert!(
        server_caps.features.contains(ServerFeature::GetPerf),
        "server must advertise GET_PERF: {server_caps:?}"
    );
    stream
}

async fn get_perf(stream: &mut UnixStream, request_id: u32, reset: bool) -> PerfReport {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetPerf { reset },
        },
    )
    .await;
    loop {
        let (type_byte, frame) = recv_typed(stream).await;
        if type_byte != TYPE_COMMAND_RESULT {
            continue;
        }
        let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
        else {
            panic!("expected COMMAND_RESULT, got {frame:?}");
        };
        assert_eq!(got, request_id);
        let CommandResult::OkWith(CommandValue::Json(json)) = result else {
            panic!("GET_PERF must answer OkWith(Json): {result:?}");
        };
        return PerfReport::from_json(&json).expect("report JSON parses");
    }
}

fn counter(report: &PerfReport, name: &str) -> u64 {
    match &report
        .metric(name)
        .unwrap_or_else(|| panic!("{name} missing"))
        .value
    {
        MetricValue::Counter(n) => *n,
        other => panic!("{name} is not a counter: {other:?}"),
    }
}

fn histogram_count(report: &PerfReport, name: &str) -> u64 {
    match &report
        .metric(name)
        .unwrap_or_else(|| panic!("{name} missing"))
        .value
    {
        MetricValue::Histogram(h) => h.count,
        other => panic!("{name} is not a histogram: {other:?}"),
    }
}

#[test]
fn get_perf_reports_hot_path_metrics_and_resets_on_request() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // A seed pane that certainly writes: the shell's own prompt is not
        // guaranteed on a non-interactive fd, `printf` is.
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("printf 'perf probe\\n'; sleep 30");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), SESSION, cmd);

        let mut stream = connect(&socket_path).await;
        send_frame(&mut stream, &attach_by_name(SESSION)).await;
        loop {
            let (type_byte, _) = recv_typed(&mut stream).await;
            if type_byte == TYPE_ATTACH_READY {
                break;
            }
        }

        // Wait for the seed's printf to travel PTY -> actor -> us.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let report = loop {
            let report = get_perf(&mut stream, 1, false).await;
            // The reader thread counts bytes before the actor applies them
            // and before the pump writes the frame to us, so wait for the
            // stage furthest downstream that the assertions below need.
            let settled = histogram_count(&report, "pty.vt_apply") > 0
                && histogram_count(&report, "wire.write") > 0;
            if settled || std::time::Instant::now() > deadline {
                break report;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(report.role, "server");
        assert_eq!(report.schema_version, phux_perf::SCHEMA_VERSION);
        assert!(report.process.is_some(), "process stats present");
        assert!(
            counter(&report, "pty.read.bytes") > 0,
            "pane output was read: {report:?}"
        );
        assert!(histogram_count(&report, "pty.read.size") > 0);
        assert!(histogram_count(&report, "pty.vt_apply") > 0);
        assert!(
            histogram_count(&report, "wire.write") > 0,
            "frames were written to us"
        );
        // Gauges reflect the registry.
        match &report.metric("proc.sessions").unwrap().value {
            MetricValue::Gauge(n) => assert_eq!(*n, 1),
            other => panic!("{other:?}"),
        }

        // reset = true zeroes the table after snapshotting it.
        let before_reset = get_perf(&mut stream, 2, true).await;
        assert!(counter(&before_reset, "pty.read.bytes") > 0);
        assert!(
            histogram_count(&before_reset, "cmd.handle") >= 1,
            "the earlier GET_PERF was timed as a command"
        );
        let after_reset = get_perf(&mut stream, 3, false).await;
        assert!(
            counter(&after_reset, "pty.read.bytes") < counter(&before_reset, "pty.read.bytes")
                || counter(&after_reset, "pty.read.bytes") == 0,
            "reset must restart the counters: before={} after={}",
            counter(&before_reset, "pty.read.bytes"),
            counter(&after_reset, "pty.read.bytes"),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
