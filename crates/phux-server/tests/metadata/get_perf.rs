//! `GET_PERF` over the wire: a JSON `PerfReport` whose hot-path metrics move
//! when a pane produces output, and `reset` restarts the counters.

use phux_perf::{MetricValue, PerfReport};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{command, join_after_shutdown, run_local, spawn_server_with_seed_cmd};

use crate::common::{attach_pane, connect_as, full_caps};

const SESSION: &str = "perf";

async fn get_perf(stream: &mut UnixStream, request_id: u32, reset: bool) -> PerfReport {
    let result = command(stream, request_id, Command::GetPerf { reset }).await;
    let CommandResult::OkWith(CommandValue::Json(json)) = result else {
        panic!("GET_PERF must answer OkWith(Json): {result:?}");
    };
    let report = PerfReport::from_json(&json).expect("report JSON parses");
    let diagnostics = report
        .stream_diagnostics
        .as_ref()
        .expect("stream diagnostics preserved");
    assert!(
        diagnostics["streams"]
            .as_array()
            .expect("stream samples")
            .len()
            <= 128
    );
    assert!(diagnostics["suppressed_streams"].as_u64().is_some());
    report
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

/// Poll `GET_PERF` until `pty.read.bytes` stops moving, so leftover PTY
/// traffic cannot inflate the post-reset snapshot.
async fn wait_until_pty_bytes_quiesce(
    stream: &mut UnixStream,
    mut request_id: u32,
    mut last_bytes: u64,
) -> u32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let report = get_perf(stream, request_id, false).await;
        request_id += 1;
        let now = counter(&report, "pty.read.bytes");
        if now == last_bytes {
            return request_id;
        }
        last_bytes = now;
        assert!(
            std::time::Instant::now() < deadline,
            "PTY byte counters never quiesced (last={last_bytes})"
        );
    }
}

#[test]
fn get_perf_reports_hot_path_metrics_and_resets_on_request() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("printf 'perf probe\\n'; sleep 30");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), SESSION, cmd);

        let (mut stream, _) = connect_as(&socket_path, "get-perf", full_caps()).await;
        attach_pane(&mut stream, SESSION).await;

        // Wait for the seed's printf to travel PTY -> actor -> us.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let report = loop {
            let report = get_perf(&mut stream, 1, false).await;
            // Wait for the furthest-downstream stage the assertions need.
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

        let next_id =
            wait_until_pty_bytes_quiesce(&mut stream, 10, counter(&report, "pty.read.bytes")).await;

        // reset = true zeroes the table after snapshotting it.
        let before_reset = get_perf(&mut stream, next_id, true).await;
        assert!(counter(&before_reset, "pty.read.bytes") > 0);
        assert!(
            histogram_count(&before_reset, "cmd.handle") >= 1,
            "the earlier GET_PERF was timed as a command"
        );
        let after_reset = get_perf(&mut stream, next_id + 1, false).await;
        assert!(
            counter(&after_reset, "pty.read.bytes") < counter(&before_reset, "pty.read.bytes")
                || counter(&after_reset, "pty.read.bytes") == 0,
            "reset must restart the counters: before={} after={}",
            counter(&before_reset, "pty.read.bytes"),
            counter(&after_reset, "pty.read.bytes"),
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
