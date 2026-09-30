//! Startup must not wait on overlay detection (ADR-0081): detection shells
//! out to `tailscale`, and running it inline left the installed server
//! unreachable for ~286ms with a live pane. The proof is ordering, not
//! timing: the injected detector blocks until the test releases it, and the
//! test releases it only after a HELLO round trip, which needs the accept
//! loop. Only the default profile auto-binds, and every test binary is a dev
//! build, so the test pins `PHUX_PROFILE=default` or it would assert nothing.

#![allow(unused_unsafe, reason = "env::set_var is unsafe only on edition 2024")]

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use phux_server::runtime::{ServerConfig, ServerRuntime};
use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, run_local, wait_for_socket,
};
use tempfile::TempDir;

/// Set when the detector is entered, so a closed gate cannot pass vacuously.
static DETECT_STARTED: AtomicBool = AtomicBool::new(false);

static DETECT_RELEASED: Mutex<bool> = Mutex::new(false);

static DETECT_RELEASE_CV: Condvar = Condvar::new();

/// Safety valve so a failing run cannot wedge runtime shutdown, which joins
/// blocking tasks.
const DETECT_CEILING: Duration = Duration::from_secs(30);

/// The detector runs on a blocking thread, so its entry is not ordered
/// against the HELLO round trip; only the non-vacuity check waits for it.
const DETECT_START_DEADLINE: Duration = Duration::from_secs(10);

/// Blocks until released; reports no address, so nothing is ever bound.
#[allow(
    clippy::significant_drop_tightening,
    reason = "a condvar wait holds its guard across the loop by construction"
)]
fn blocking_detect() -> Vec<IpAddr> {
    DETECT_STARTED.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + DETECT_CEILING;
    let mut released = DETECT_RELEASED.lock().expect("detect barrier");
    while !*released {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let (guard, timed_out) = DETECT_RELEASE_CV
            .wait_timeout(released, remaining)
            .expect("detect barrier wait");
        released = guard;
        if timed_out.timed_out() {
            break;
        }
    }
    Vec::new()
}

fn release_detect() {
    {
        let mut released = DETECT_RELEASED.lock().expect("detect barrier");
        *released = true;
    }
    DETECT_RELEASE_CV.notify_all();
}

/// Releases the detector on unwind too, so a failure cannot park a task.
struct ReleaseOnDrop;

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        release_detect();
    }
}

fn detect_released() -> bool {
    *DETECT_RELEASED.lock().expect("detect barrier")
}

#[test]
fn server_serves_clients_while_overlay_detection_is_still_running() {
    // SAFETY: no other thread exists yet; nextest runs one test per process.
    unsafe { std::env::set_var("PHUX_PROFILE", phux_config::instance::DEFAULT_PROFILE) };

    run_local(async {
        let _release_on_unwind = ReleaseOnDrop;

        let dir = TempDir::new().expect("tempdir");
        let socket_path = dir.path().join("phux.sock");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let cfg = ServerConfig {
            socket_path: socket_path.clone(),
            pre_seeded_session: Some("overlay-startup".to_owned()),
            seed_with_pty: false,
            seed_command: None,
            ..ServerConfig::with_default_socket()
        };
        let server = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .overlay_detect(blocking_detect)
                .run_async(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        // HELLO completes only via the accept loop, while detection is blocked.
        let started = Instant::now();
        let stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let served_in = started.elapsed();
        assert!(
            !detect_released(),
            "the test released the detector before the server answered; the \
             assertion below would prove nothing",
        );

        let start_deadline = Instant::now() + DETECT_START_DEADLINE;
        while !DETECT_STARTED.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < start_deadline,
                "overlay detection never ran, so nothing here was exercised. \
                 The auto-listen gate is closed on this machine: check \
                 PHUX_PROFILE.",
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !detect_released(),
            "detection returned on its own; the barrier is not holding and \
             the ordering claim is unproven (served in {served_in:?})",
        );

        // Detection finishing (with nothing to bind) must not stop the server.
        release_detect();
        let second = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        drop(second);
        drop(stream);
        shutdown_tx.send(()).expect("shutdown receiver alive");
        tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
            .await
            .expect("server joined within deadline")
            .expect("server task did not panic")
            .expect("server exited cleanly");
    });
}
