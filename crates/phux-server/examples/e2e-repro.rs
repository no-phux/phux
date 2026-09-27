//! `e2e-repro`: a one-command, real-server repro of a lag/crash edge case.
//!
//! Spins a real server with a PTY pane, attaches a client, drives heavy
//! colored output, a second client attaching mid-stream, a resize storm, and
//! a line of input, then writes screen snapshots to `/tmp/phux-repro-<ts>/`.
//! Run with `PHUX_LOG=/tmp/phux-repro.jsonl PHUX_LOG_FORMAT=json
//! RUST_LOG=phux=debug` to also capture a server trace.
//!
//!   cargo run -p phux-server --example e2e-repro

#![allow(
    clippy::print_stderr,
    clippy::expect_used,
    reason = "standalone diagnostic harness"
)]

use std::path::Path;

use phux_protocol::wire::frame::ViewportInfo;
use phux_server_testkit::builder::{DEFAULT_IDLE_MS, E2eBuilder};
use portable_pty::CommandBuilder;

/// Ten in-place repaints of an 80x40 grid with one 256-color SGR per cell
/// (the worst-case colored shape), then `stty size` in a loop so the resize
/// storm is observable, then idle.
const SCENARIO: &str = "sleep 0.2; cols=80; rows=40; \
    for g in $(seq 1 10); do printf '\\033[H'; \
    r=1; while [ \"$r\" -le \"$rows\" ]; do \
    line=''; c=1; while [ \"$c\" -le \"$cols\" ]; do \
    n=$(( 16 + (c + r + g) % 216 )); line=\"$line\\033[38;5;${n}mX\"; c=$(( c + 1 )); done; \
    printf \"%b\\033[0m\\r\\n\" \"$line\"; r=$(( r + 1 )); done; done; \
    printf '\\033[0mBURST_DONE\\r\\n'; \
    for i in $(seq 1 60); do stty size; sleep 0.05; done; \
    printf 'SCRIPT_DONE\\r\\n'; sleep 30";

fn write_snapshot(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write snapshot");
}

fn main() {
    let _log_guard = phux_server::telemetry::init().ok().flatten();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let out_dir = std::env::temp_dir().join(format!("phux-repro-{ts}"));
    std::fs::create_dir_all(&out_dir).expect("create artifact dir");
    eprintln!("[e2e-repro] artifacts -> {}", out_dir.display());

    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", SCENARIO]);
    phux_server_testkit::run_local(async {
        let mut harness = E2eBuilder::new()
            .seed_cmd(cmd)
            .viewport(80, 40)
            .spawn()
            .await;
        let c1 = &mut harness.clients[0];
        let _ = c1.wait_until(|s| s.contains("BURST_DONE")).await;
        write_snapshot(&out_dir, "01-after-burst.txt", &c1.snapshot_text());

        let mut c2 = harness.attach_client(ViewportInfo::new(80, 40)).await;
        c2.converge(DEFAULT_IDLE_MS).await;
        write_snapshot(&out_dir, "02-client2-attach.txt", &c2.snapshot_text());

        let c1 = &mut harness.clients[0];
        for (cols, rows) in [
            (100, 30),
            (120, 45),
            (90, 50),
            (140, 38),
            (110, 42),
            (128, 44),
        ] {
            c1.resize(cols, rows).await;
        }
        // `stty size` prints `<rows> <cols>`.
        let _ = c1.wait_until(|s| s.contains("44 128")).await;
        write_snapshot(&out_dir, "03-after-resize-storm.txt", &c1.snapshot_text());

        c1.send_text("echo hello-from-repro\r").await;
        c1.drain_output_bounded(64).await;
        write_snapshot(&out_dir, "04-after-input.txt", &c1.snapshot_text());

        drop(c2);
        harness.shutdown().await;
    });
    eprintln!("[e2e-repro] done. snapshots in {}", out_dir.display());
}
