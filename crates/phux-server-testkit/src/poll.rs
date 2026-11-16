//! Bounded polling for conditions with no delivery signal of their own.
//!
//! The deadline is always the caller's: each suite keeps its own constant
//! ([`crate::WIRE_RECV_TIMEOUT`], a scenario's `DEADLINE`, ...). Both waits
//! measure wall-clock time, check before the first sleep, and panic with the
//! same message: what was awaited and for how long.

use std::future::Future;
use std::time::{Duration, Instant};

/// Poll `observe` every `poll` until it yields a value, and return it.
///
/// # Panics
///
/// When `deadline` passes first; the message names `what`.
pub async fn eventually<T>(
    what: &str,
    deadline: Duration,
    poll: Duration,
    mut observe: impl FnMut() -> Option<T>,
) -> T {
    let start = Instant::now();
    loop {
        if let Some(found) = observe() {
            return found;
        }
        expire_after(start, deadline, what);
        tokio::time::sleep(poll).await;
    }
}

/// [`eventually`] for an observation that has to await (a file read, a
/// probe dial).
///
/// # Panics
///
/// When `deadline` passes first; the message names `what`.
pub async fn eventually_async<T, F>(
    what: &str,
    deadline: Duration,
    poll: Duration,
    mut observe: impl FnMut() -> F,
) -> T
where
    F: Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(found) = observe().await {
            return found;
        }
        expire_after(start, deadline, what);
        tokio::time::sleep(poll).await;
    }
}

fn expire_after(start: Instant, deadline: Duration, what: &str) {
    assert!(
        start.elapsed() < deadline,
        "timed out after {deadline:?} waiting for {what}"
    );
}
