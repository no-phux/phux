//! Prove the adopted trait objects drive a *real* PTY when handed only a raw
//! master fd + child PID — the phux-specific risk behind ADR-0032's fd
//! re-adoption (no `execve` needed to test this half).
//!
//! We open a genuine PTY with `portable-pty`, spawn a shell on the slave, then
//! reconstruct [`AdoptedMaster`]/[`AdoptedChild`] from `(raw_fd, pid)` alone
//! and exercise the full surface phux depends on: read, write, resize, and
//! child reaping.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use portable_pty_adopt::{AdoptedChild, AdoptedMaster};
use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::mpsc;
use std::time::Duration;

/// Ceiling for "the adopted child should have echoed by now" waits.
///
/// Not load-bearing: the assertion is that the needle appears at all, never
/// how fast. A real PTY child on a saturated box can be descheduled for
/// seconds, so this is sized to be unreachable rather than hand-picked
/// (phux-br1f); a child that never writes still fails the run, just later.
const OUTPUT_DEADLINE: Duration = Duration::from_secs(30);

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

    let deadline = std::time::Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(seen) if seen.contains(needle) => return true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    false
}

#[test]
fn adopts_master_and_child_from_fd_and_pid() {
    // 1. A real PTY + a real child, the normal portable-pty way.
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
    // Echo each line back with a marker; quit on a sentinel.
    cmd.arg("while IFS= read -r line; do echo \"got:$line\"; done");
    let child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave); // child holds the only slave fd now.

    let pid = i32::try_from(child.process_id().unwrap()).unwrap();
    let master_fd = pair.master.as_raw_fd().unwrap();

    // 2. Recover the master as a *bare* fd (dup, as if inherited across
    //    execve), then throw away every portable-pty handle. From here on the
    //    only state we have is `(dup_fd, pid)` — exactly the resume situation.
    // SAFETY: `dup` returns a fresh owned descriptor for the same master.
    let dup_fd = unsafe { libc::dup(master_fd) };
    assert!(
        dup_fd >= 0,
        "dup failed: {}",
        std::io::Error::last_os_error()
    );
    drop(pair.master);
    drop(child); // std Child drop does not reap; we become the sole reaper.

    // SAFETY: `dup_fd` is an owned PTY master descriptor we just created.
    let master = AdoptedMaster::new(unsafe { OwnedFd::from_raw_fd(dup_fd) });
    let mut adopted_child = AdoptedChild::new(pid);

    // 3. The child survived losing portable-pty's handles: still running.
    assert!(
        matches!(adopted_child.try_wait(), Ok(None)),
        "adopted child should be alive"
    );

    // 4. Write through the adopted master, read the echo back through it.
    let reader = master.try_clone_reader().unwrap();
    {
        let mut writer = master.take_writer().unwrap();
        writer.write_all(b"hello\n").unwrap();
        writer.flush().unwrap();
    }
    assert!(
        read_until(reader, "got:hello", OUTPUT_DEADLINE),
        "expected the child's echo through the adopted master"
    );

    // 5. Resize via the adopted master takes effect (TIOCSWINSZ/TIOCGWINSZ).
    master
        .resize(PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let size = master.get_size().unwrap();
    assert_eq!((size.rows, size.cols), (40, 100));

    // 6. process_group_leader resolves to a real pgrp on the live tty.
    assert!(master.process_group_leader().is_some());

    // 7. Kill + reap through the adopted child.
    adopted_child.kill().unwrap();
    let status = adopted_child.wait().unwrap();
    assert!(!status.success(), "SIGKILL is not a success exit");
    // Cached: a second wait returns the same status, not an ECHILD error.
    assert!(adopted_child.wait().is_ok());
}
