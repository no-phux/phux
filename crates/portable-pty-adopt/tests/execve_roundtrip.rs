//! End-to-end proof of the bet behind ADR-0032: a PTY master fd (with
//! `FD_CLOEXEC` cleared) and the child attached to it both survive a real
//! `execve` of the parent, and the new image can re-adopt them via this crate.
//!
//! Structure — the single test re-execs itself through three stages, selected
//! by `PPA_ROUNDTRIP_STAGE`:
//!
//! - **orchestrate** (no env): spawn a fresh copy of this test binary as the
//!   `lead` stage and assert it prints the pass marker.
//! - **lead**: open a real PTY, spawn a ticking child on the slave, clear
//!   `FD_CLOEXEC` on the master, then `execve` this binary again as `resume`,
//!   handing off the master fd number + child PID via env. `execve` replaces
//!   the process in place, so the child stays alive and stays our child.
//! - **resume**: the post-exec image adopts the inherited fd + PID with
//!   [`AdoptedMaster`]/[`AdoptedChild`], confirms the child is still running
//!   and its bytes still flow through the inherited master, then exits.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic
)]

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use portable_pty_adopt::{AdoptedChild, AdoptedMaster};
use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Ceiling for "the adopted child should have echoed by now" waits.
///
/// Not load-bearing: the assertion is that the needle appears at all, never
/// how fast. A real PTY child on a saturated box can be descheduled for
/// seconds, so this is sized to be unreachable rather than hand-picked
/// (phux-br1f); a child that never writes still fails the run, just later.
const OUTPUT_DEADLINE: Duration = Duration::from_secs(30);

const STAGE: &str = "PPA_ROUNDTRIP_STAGE";
const FD: &str = "PPA_ROUNDTRIP_FD";
const PID: &str = "PPA_ROUNDTRIP_PID";
const TEST: &str = "child_and_master_fd_survive_execve";
const MARKER: &str = "ROUNDTRIP_PASS";

#[test]
fn child_and_master_fd_survive_execve() {
    match std::env::var(STAGE).ok().as_deref() {
        None => orchestrate(),
        Some("lead") => lead(),
        Some("resume") => resume(),
        Some(other) => panic!("unknown stage {other}"),
    }
}

/// Stage 0: run a fresh copy of this binary as the `lead` stage, capture its
/// output, and assert it reported success.
fn orchestrate() {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(STAGE, "lead")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains(MARKER),
        "execve round-trip failed (status {:?})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        out.status,
    );
}

/// Stage 1: own a PTY + child, then `execve` into the `resume` stage with the
/// master fd and child PID handed off via env. Never returns on success.
fn lead() -> ! {
    let sys = native_pty_system();
    let pair = sys
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new("sh");
    cmd.arg("-c");
    cmd.arg("while true; do printf 'TICK\\n'; sleep 0.1; done");
    let child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let pid = child.process_id().unwrap();
    let master_fd = pair.master.as_raw_fd().unwrap();
    clear_cloexec(master_fd);

    // Leak the portable-pty handles: their Drop would close the master
    // (EOF-ing the child) and we want the bare fd to survive untouched across
    // the exec. The child stays alive regardless — it's a separate process.
    std::mem::forget(pair.master);
    std::mem::forget(child);

    let exe = std::env::current_exe().unwrap();
    let err = Command::new(exe)
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(STAGE, "resume")
        .env(FD, master_fd.to_string())
        .env(PID, pid.to_string())
        .exec(); // replaces this process image in place
    panic!("execve into resume stage failed: {err}");
}

/// Stage 2 (post-exec image): re-adopt the inherited fd + PID and verify the
/// child survived our parent's `execve` and still drives the master.
fn resume() -> ! {
    let fd: RawFd = std::env::var(FD).unwrap().parse().unwrap();
    let pid: libc::pid_t = std::env::var(PID).unwrap().parse().unwrap();

    // SAFETY: `fd` is the inherited PTY master (FD_CLOEXEC cleared by `lead`),
    // owned solely by this process now.
    let master = unsafe { AdoptedMaster::from_raw_fd(fd) };
    let mut child = AdoptedChild::new(pid);

    let alive = matches!(child.try_wait(), Ok(None));
    let reader = master.try_clone_reader().unwrap();
    let flowing = read_until(reader, "TICK", OUTPUT_DEADLINE);

    // Don't leak the ticking child out of the test run.
    let _ = child.kill();
    let _ = child.wait();

    let mut stdout = std::io::stdout();
    if alive && flowing {
        let _ = writeln!(stdout, "{MARKER}");
    } else {
        let _ = writeln!(stdout, "ROUNDTRIP_FAIL alive={alive} flowing={flowing}");
    }
    let _ = stdout.flush();
    std::process::exit(i32::from(!(alive && flowing)));
}

fn clear_cloexec(fd: RawFd) {
    // SAFETY: F_GETFD/F_SETFD on a descriptor we own; no memory concerns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        assert!(flags != -1, "F_GETFD: {}", std::io::Error::last_os_error());
        let rc = libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        assert!(rc != -1, "F_SETFD: {}", std::io::Error::last_os_error());
    }
}

fn read_until<R: std::io::Read + Send + 'static>(
    mut reader: R,
    needle: &str,
    timeout: Duration,
) -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut acc = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    acc.extend_from_slice(&buf[..n]);
                    if tx.send(String::from_utf8_lossy(&acc).into_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(seen) if seen.contains(needle) => return true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    false
}
