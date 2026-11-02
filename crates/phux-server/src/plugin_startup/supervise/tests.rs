use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{Supervisor, serve_iterations};
use crate::plugin_startup::rotate::CHECK_INTERVAL;

struct ClearDefer;
impl Drop for ClearDefer {
    fn drop(&mut self) {
        crate::plugin_startup::spawn::DEFER_REAP_PID.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Real-time bound. Tokio time is paused in the caller, so this yields
/// instead of sleeping on the runtime clock.
async fn until_real(limit: std::time::Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if pred() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::task::yield_now().await;
    }
}

#[test]
fn empty_catalog_arms_no_timer() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        tokio::time::pause();
        let root = CancellationToken::new();
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let supervisor = Supervisor {
            root: root.clone(),
            socket: PathBuf::from("/tmp/phux-empty.sock"),
            state_dir: PathBuf::from("/tmp"),
            run_log: PathBuf::from("/tmp/plugin-runs.jsonl"),
            reload: None,
            catalog: Vec::new(),
            slots: Vec::new(),
            paused: false,
            logged_skips: BTreeSet::new(),
            origin: Instant::now(),
            rotate_at: Instant::now() + CHECK_INTERVAL,
            blocked: false,
            notify: Arc::new(Notify::new()),
        };
        serve_iterations().store(0, Ordering::Relaxed);
        let task = tokio::task::spawn_local(async move {
            let mut supervisor = supervisor;
            supervisor.serve(&mut rx).await;
        });
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let parked = serve_iterations().load(Ordering::Relaxed);
        assert!(parked >= 1, "the supervisor should enter its loop");
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            serve_iterations().load(Ordering::Relaxed),
            parked,
            "an empty catalog must not arm a timer"
        );
        root.cancel();
        task.await.unwrap();
    });
}

/// A fired exit watch whose first `try_wait` misses the zombie must be
/// polled again on the one-second fallback. That only happens because
/// the watch stores `fired` before it notifies.
///
/// Virtual time stays put until the deferred wait has actually happened.
/// Advancing first, before `/bin/true` has exited, jumps the clock with
/// no fallback armed yet, and the later one-second sleep is then still
/// in the future.
#[test]
fn fired_watch_is_polled_again_when_the_first_wait_misses() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        tokio::time::pause();
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&root_dir).unwrap();
        let run_log = tmp.path().join("plugin-runs.jsonl");
        let root = CancellationToken::new();
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let mut supervisor = Supervisor {
            root: root.clone(),
            socket: tmp.path().join("phux.sock"),
            state_dir: tmp.path().join("state"),
            run_log: run_log.clone(),
            reload: None,
            catalog: Vec::new(),
            slots: Vec::new(),
            paused: false,
            logged_skips: BTreeSet::new(),
            origin: Instant::now(),
            rotate_at: Instant::now() + CHECK_INTERVAL,
            blocked: false,
            notify: Arc::new(Notify::new()),
        };
        supervisor.start_fresh(crate::plugin_startup::StartupSpec {
            plugin_id: "plug".to_owned(),
            plugin_name: "Plug".to_owned(),
            plugin_version: "0.1.0".to_owned(),
            startup_id: "web".to_owned(),
            command: vec!["/bin/true".to_owned()],
            plugin_root: root_dir,
            manifest_hash: 1,
        });
        let child = supervisor.slots[0].child.as_ref().unwrap();
        let pid = child.pid().expect("sidecar pid");
        assert!(
            child.has_exit_watch(),
            "the exit watch must be armed before it fires"
        );
        crate::plugin_startup::spawn::DEFER_REAP_PID.store(pid, Ordering::Relaxed);
        let _clear = ClearDefer;
        let task = tokio::task::spawn_local(async move {
            supervisor.serve(&mut rx).await;
        });
        let missed = until_real(std::time::Duration::from_secs(5), || {
            crate::plugin_startup::spawn::DEFER_REAP_PID.load(Ordering::Relaxed) == 0
        })
        .await;
        let early = std::fs::read_to_string(&run_log).unwrap_or_default();
        assert!(
            missed,
            "the first wait never missed the exit within 5s; log={early}"
        );
        assert!(
            !early.contains("exited 0"),
            "the missed wait must not count as reaped yet: {early}"
        );
        // The poll that consumed the defer runs to the next `select`
        // before this task is scheduled again, so the one-second
        // fallback is already armed. Jump past that second and stop
        // short of the three-second rotate deadline.
        tokio::time::advance(std::time::Duration::from_millis(1500)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let later = std::fs::read_to_string(&run_log).unwrap_or_default();
        assert!(
            later.contains("exited 0"),
            "the 1s fallback should observe the exit after the watch fired: {later}"
        );
        tokio::time::resume();
        root.cancel();
        task.await.unwrap();
    });
}
