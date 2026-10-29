//! Stop every named sidecar together: one `SIGTERM`, one shared deadline,
//! then `SIGKILL`. Server exit stays within the grace no matter how many
//! sidecars are running.

use std::time::{Duration, Instant};

use super::spawn::GroupChild;

/// How long a sidecar has to exit after `SIGTERM` before the group is killed.
pub(super) const STOP_GRACE: Duration = Duration::from_secs(5);

/// Signal `children` in parallel and wait until they are gone or the grace
/// plus a short reap has elapsed.
pub(super) async fn stop_children(children: &mut [GroupChild]) {
    if children.is_empty() {
        return;
    }
    for child in children.iter() {
        child.signal(libc::SIGTERM);
    }
    wait_until(STOP_GRACE, children).await;
    for child in children.iter_mut() {
        if !child.poll_reaped() {
            child.signal(libc::SIGKILL);
        }
    }
    wait_until(Duration::from_secs(2), children).await;
    for child in children.iter() {
        let _ = std::fs::remove_file(&child.pidfile);
    }
}

async fn wait_until(limit: Duration, children: &mut [GroupChild]) {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if all_reaped(children) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll every child. `Iterator::all` would skip the rest after the first
/// one still running, and those exits would wait for the next pass.
fn all_reaped(children: &mut [GroupChild]) -> bool {
    let mut done = true;
    for child in children {
        if !child.poll_reaped() {
            done = false;
        }
    }
    done
}
