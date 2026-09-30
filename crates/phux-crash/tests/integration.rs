//! Integration tests for phux-crash.
//!
//! These tests verify that installing the handler does not interfere with
//! normal program operation (tokio runtime, signal handling, I/O), that a
//! fatal signal still kills the process with that signal after the handler
//! runs, and that the terminal restore bytes reach stderr only while armed.
//!
//! Tests that send fatal signals use subprocess isolation: the test process
//! re-executes itself with an env var that selects the crash scenario, so
//! the parent can verify outcomes without dying.

#![cfg(unix)]

use std::process::Command;

/// Re-invoke the current test binary as a subprocess with the given scenario.
/// Returns (exit status, stdout, stderr).
fn run_scenario(scenario: &str) -> (std::process::ExitStatus, String, String) {
    let exe = std::env::current_exe().expect("current_exe");
    let output = Command::new(exe)
        .env("CRASH_TEST_SCENARIO", scenario)
        .arg("--ignored")
        .arg("--exact")
        .arg("--nocapture")
        .arg("subprocess_entry")
        .output()
        .expect("failed to spawn subprocess");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ── Subprocess entry point ──────────────────────────────────────────────

/// This test is `#[ignore]`d so it only runs when invoked as a subprocess
/// by the parent test via `run_scenario`. The `CRASH_TEST_SCENARIO` env
/// var selects which scenario to execute.
#[test]
#[ignore]
fn subprocess_entry() {
    let scenario = match std::env::var("CRASH_TEST_SCENARIO") {
        Ok(s) => s,
        Err(_) => return, // not a subprocess invocation
    };

    // Install the handler before anything else.
    phux_crash::install_terminal_restore_only();

    match scenario.as_str() {
        // Scenario 1: install handler, run tokio runtime with concurrent work, exit cleanly.
        "tokio_normal" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async {
                // Spawn several concurrent tasks to stress the runtime.
                let mut handles = Vec::new();
                for i in 0..20 {
                    handles.push(tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        i * i
                    }));
                }
                let mut sum = 0u64;
                for h in handles {
                    sum += h.await.unwrap();
                }
                // Also test signal infrastructure coexistence.
                // Register a tokio SIGTERM handler (same as the pager does).
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{SignalKind, signal};
                    let _term = signal(SignalKind::terminate())
                        .expect("tokio SIGTERM handler should work alongside crash handler");
                }
                eprintln!("tokio_normal: sum={sum}, all tasks completed");
            });
        }

        // Scenario 2: install handler, do sync file I/O and computation, exit cleanly.
        "sync_normal" => {
            let tmp = tempfile::tempdir().expect("tempdir");
            for i in 0..50 {
                let path = tmp.path().join(format!("file-{i}.txt"));
                std::fs::write(&path, format!("contents {i}")).expect("write");
                let data = std::fs::read_to_string(&path).expect("read");
                assert!(data.contains(&format!("{i}")));
            }
            eprintln!("sync_normal: 50 files written and read back");
        }

        // Scenario 3: install handler, send ourselves SIGBUS.
        "sigbus" => {
            // SAFETY: raising a signal has no memory-safety preconditions.
            unsafe { libc::raise(libc::SIGBUS) };
        }

        // Scenario 4: install handler, send ourselves SIGSEGV.
        "sigsegv" => {
            // SAFETY: raising a signal has no memory-safety preconditions.
            unsafe { libc::raise(libc::SIGSEGV) };
        }

        // Scenario 4b (ADDED FOR PHUX): arm escape-code restoration the way
        // `RawModeGuard` does, then crash.
        "sigsegv_tui" => {
            phux_crash::enable_terminal_escape_restore();
            // SAFETY: raising a signal has no memory-safety preconditions.
            unsafe { libc::raise(libc::SIGSEGV) };
        }

        // Scenario 4c (ADDED FOR PHUX): arm and then DISARM escape
        // restoration, mirroring `RawModeGuard::drop`. A crash after the alt
        // screen is gone must not spray DECSETs across the normal screen.
        "sigsegv_after_disable" => {
            phux_crash::enable_terminal_escape_restore();
            phux_crash::disable_terminal_escape_restore();
            // SAFETY: raising a signal has no memory-safety preconditions.
            unsafe { libc::raise(libc::SIGSEGV) };
        }

        // Scenario 6: install handler, abort. This is the path every Rust
        // panic takes in release builds (panic = "abort" → SIGABRT).
        "sigabrt" => {
            std::process::abort();
        }

        // Scenario 5: tokio runtime + signal coexistence, then clean shutdown.
        "tokio_signals" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async {
                use tokio::signal::unix::{SignalKind, signal};
                let mut usr1 = signal(SignalKind::user_defined1()).expect("SIGUSR1 handler");

                // Send ourselves SIGUSR1 and verify tokio receives it
                // (proves our SIGBUS/SIGSEGV handler doesn't clobber other signals).
                // SAFETY: raising a signal has no memory-safety preconditions.
                unsafe { libc::raise(libc::SIGUSR1) };
                tokio::time::timeout(std::time::Duration::from_secs(2), usr1.recv())
                    .await
                    .expect("SIGUSR1 should arrive within 2s");

                eprintln!("tokio_signals: SIGUSR1 received, signal coexistence OK");
            });
        }

        other => {
            eprintln!("unknown scenario: {other}");
            std::process::exit(99);
        }
    }
}

// ── Parent test cases ───────────────────────────────────────────────────

#[test]
fn handler_does_not_interfere_with_tokio_runtime() {
    let (status, _stdout, stderr) = run_scenario("tokio_normal");
    assert!(
        status.success(),
        "tokio_normal should exit 0, got {status:?}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("all tasks completed"),
        "should see completion message\nstderr: {stderr}"
    );
}

#[test]
fn handler_does_not_interfere_with_sync_io() {
    let (status, _stdout, stderr) = run_scenario("sync_normal");
    assert!(
        status.success(),
        "sync_normal should exit 0, got {status:?}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("50 files written"),
        "should see completion message\nstderr: {stderr}"
    );
}

#[test]
fn handler_does_not_clobber_other_signal_handlers() {
    let (status, _stdout, stderr) = run_scenario("tokio_signals");
    assert!(
        status.success(),
        "tokio_signals should exit 0, got {status:?}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("signal coexistence OK"),
        "SIGUSR1 should be delivered through tokio\nstderr: {stderr}"
    );
}

/// Run `scenario` and assert the child died of `expected` — the handler must
/// restore `SIG_DFL` and re-raise, not exit or swallow the signal.
fn assert_killed_by(scenario: &str, expected: libc::c_int) {
    use std::os::unix::process::ExitStatusExt;

    let (status, _stdout, stderr) = run_scenario(scenario);
    assert_eq!(
        status.signal(),
        Some(expected),
        "{scenario}: process should be killed by signal {expected}, got {status:?}\nstderr: {stderr}"
    );
}

#[test]
fn sigbus_is_reraised_with_default_disposition() {
    assert_killed_by("sigbus", libc::SIGBUS);
}

#[test]
fn sigsegv_is_reraised_with_default_disposition() {
    assert_killed_by("sigsegv", libc::SIGSEGV);
}

/// `panic = "abort"` release builds die through this path.
#[test]
fn sigabrt_is_reraised_with_default_disposition() {
    assert_killed_by("sigabrt", libc::SIGABRT);
}

/// ADDED FOR PHUX. The whole reason this crate exists: when the client
/// dies on a fatal signal with the alt screen up, the terminal must be handed
/// back usable. Asserts the real bytes reach fd 2 from signal context.
#[test]
fn fatal_signal_writes_restore_sequence_to_stderr() {
    let (_status, _stdout, stderr) = run_scenario("sigsegv_tui");

    let restore = phux_crash::terminal::RESTORE_SEQ;
    let restore_str = std::str::from_utf8(restore).expect("RESTORE_SEQ is UTF-8");
    assert!(
        stderr.contains(restore_str),
        "a fatal signal with escape restore armed must emit the full restore \
         sequence to stderr; got {stderr:?}"
    );

    // Spot-check the modes that actually wedge a phux user, so a future edit
    // to RESTORE_SEQ that drops one fails here and not in someone's terminal.
    for (mode, what) in [
        ("\x1b[?1049l", "leave alt screen"),
        ("\x1b[?25h", "show cursor"),
        ("\x1b[?1002l", "button-event mouse tracking off"),
        ("\x1b[?1006l", "SGR mouse reporting off"),
        ("\x1b[?1003l", "any-motion mouse tracking off"),
        ("\x1b[?2026l", "end synchronized update"),
    ] {
        assert!(
            stderr.contains(mode),
            "restore sequence must {what} ({mode:?})"
        );
    }
}

/// ADDED FOR PHUX. The mirror of the above: after `RawModeGuard::drop` has
/// disarmed escape restoration, a crash must leave the normal screen alone.
#[test]
fn fatal_signal_after_disable_writes_no_escape_codes() {
    let (_status, _stdout, stderr) = run_scenario("sigsegv_after_disable");

    assert!(
        !stderr.contains("\x1b[?1049l"),
        "with escape restore disarmed, a crash must not emit DECSET resets; \
         got {stderr:?}"
    );
}
