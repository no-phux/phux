//! Counter tripwires for a 16 MiB pane flood.
//!
//! Counts, not wall time: main-loop handoffs per MiB the server actually
//! read, plus `pump.gap_resync` and the neighbouring stall counters. The
//! ceilings sit well above a quiet run of current main so a loaded runner
//! does not flake, and below a per-line wakeup storm.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use phux_perf::PerfReport;
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{join_after_shutdown, run_local, spawn_server_with_seed_cmd};

use super::get_perf::{counter, get_perf, histogram_count};
use crate::common::{attach_pane, connect_as, full_caps};

const SESSION: &str = "flood";
const FLOOD_BYTES: u64 = 16 * 1024 * 1024;

/// Handoffs per MiB (`pty.burst.bytes` samples). A quiet run of this flood
/// on `x86_64` was a few hundred to about 700: the actor keeps up, so the coalesce cap rarely
/// fills and each ~1.5 KiB read is its own wakeup. A per-line storm is an
/// order of magnitude past this ceiling.
const MAX_HANDOFFS_PER_MIB: u64 = 4_000;
/// In-band resyncs for the whole flood. A keeping-up client stays near zero;
/// the ceiling leaves room for a loaded runner.
const MAX_GAP_RESYNC: u64 = 256;
/// Lagged broadcast receivers. Same shape as gap resyncs.
const MAX_PUMP_LAGGED: u64 = 256;
/// Times the reader blocked on a full actor queue. Load pushes this up;
/// a stuck reader sits in the thousands.
const MAX_READER_BLOCKED: u64 = 1_024;

fn write_flood(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("flood.txt");
    let mut file = std::fs::File::create(&path).expect("flood file");
    let line = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
    let mut left = usize::try_from(FLOOD_BYTES).expect("flood fits");
    while left > 0 {
        let n = line.len().min(left);
        file.write_all(&line[..n]).expect("write flood");
        left -= n;
    }
    file.flush().expect("flush flood");
    path
}

async fn snapshot_when_idle(stream: &mut UnixStream, mut request_id: u32) -> (PerfReport, u32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut last = 0_u64;
    let mut stable = 0_u32;
    loop {
        let report = get_perf(stream, request_id, false).await;
        request_id += 1;
        let now = counter(&report, "pty.read.bytes");
        if now == last && now >= FLOOD_BYTES {
            stable += 1;
            if stable >= 2 {
                return (report, request_id);
            }
        } else {
            stable = 0;
            last = now;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "PTY counters never settled at {FLOOD_BYTES} bytes (last={last})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn flood_16mib_stays_under_counter_ceilings() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let flood = write_flood(tmp.path());
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        // A login shell would source the host profile and paint a prompt
        // over the flood. This child only needs cat and sleep.
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("HOME", tmp.path());
        cmd.env("TERM", "xterm-256color");
        cmd.arg("-c");
        cmd.arg(format!("/bin/cat '{}'; /bin/sleep 120", flood.display()));
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), SESSION, cmd);

        let (mut stream, _) = connect_as(&socket_path, "flood", full_caps()).await;
        attach_pane(&mut stream, SESSION).await;

        // `GET_PERF` reads past terminal frames, so polling it is what keeps
        // this client caught up. The completion signal is the byte counter:
        // a marker in the VT stream can be scrolled off or dropped on a gap.
        let (report, _) = snapshot_when_idle(&mut stream, 1).await;
        let read = counter(&report, "pty.read.bytes");
        let handoffs = histogram_count(&report, "pty.burst.bytes");
        let gap = counter(&report, "pump.gap_resync");
        let lagged = counter(&report, "pump.lagged");
        let blocked = counter(&report, "pty.reader.blocked");
        let per_mib = handoffs.saturating_mul(1024 * 1024) / read;
        assert!(
            read >= FLOOD_BYTES,
            "server read {read} bytes, under the {FLOOD_BYTES}-byte flood"
        );
        assert!(
            per_mib <= MAX_HANDOFFS_PER_MIB,
            "{per_mib} main-loop handoffs per MiB exceeds {MAX_HANDOFFS_PER_MIB}"
        );
        assert!(
            gap <= MAX_GAP_RESYNC,
            "pump.gap_resync {gap} exceeds {MAX_GAP_RESYNC}"
        );
        assert!(
            lagged <= MAX_PUMP_LAGGED,
            "pump.lagged {lagged} exceeds {MAX_PUMP_LAGGED}"
        );
        assert!(
            blocked <= MAX_READER_BLOCKED,
            "pty.reader.blocked {blocked} exceeds {MAX_READER_BLOCKED}"
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
