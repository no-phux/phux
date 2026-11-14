//! Find the overlay address the auto-bound listeners need. A server started
//! at boot can run before the overlay is up, so a miss is retried with the
//! bind retry's capped backoff, and sooner when an interface address is
//! added, until an address appears or the server stops.

use super::addr_events::AddrEvents;
use super::retry::Backoff;
use std::net::IpAddr;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Run `detect` (it shells out, so on a blocking thread) until it reports an
/// address, and return the first one. `on_first_miss` runs once, when the
/// first attempt finds nothing. `None` means the server is shutting down.
pub(super) async fn detect_until_found(
    detect: fn() -> Vec<IpAddr>,
    on_first_miss: impl FnOnce(),
    token: &CancellationToken,
) -> Option<IpAddr> {
    let mut on_first_miss = Some(on_first_miss);
    let mut events: Option<AddrEvents> = None;
    let mut backoff = Backoff::new();
    loop {
        let found = tokio::select! {
            () = token.cancelled() => return None,
            joined = tokio::task::spawn_blocking(detect) => joined.unwrap_or_else(|err| {
                warn!(error = %err, "overlay detection task failed; retrying");
                Vec::new()
            }),
        };
        if let Some(ip) = found.into_iter().next() {
            return Some(ip);
        }
        if let Some(first_miss) = on_first_miss.take() {
            first_miss();
        }
        let events = events.get_or_insert_with(AddrEvents::subscribe);
        tokio::select! {
            () = token.cancelled() => return None,
            () = tokio::time::sleep(backoff.next_delay()) => {}
            () = events.changed() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::time::Instant;

    static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

    fn appears_on_fifth_try() -> Vec<IpAddr> {
        if ATTEMPTS.fetch_add(1, Ordering::SeqCst) < 4 {
            Vec::new()
        } else {
            vec![IpAddr::from([100, 64, 0, 2]), IpAddr::from([100, 64, 0, 3])]
        }
    }

    fn never_found() -> Vec<IpAddr> {
        Vec::new()
    }

    #[tokio::test(start_paused = true)]
    async fn a_late_address_is_found_by_retrying_with_backoff() {
        let start = Instant::now();
        let mut misses = 0;
        let found = detect_until_found(
            appears_on_fifth_try,
            || misses += 1,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(found, Some(IpAddr::from([100, 64, 0, 2])));
        assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 5);
        assert_eq!(misses, 1, "the miss is reported once, not per attempt");
        // 1 + 2 + 4 + 8 seconds of backoff between the five attempts.
        assert!(start.elapsed() >= Duration::from_secs(15));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_ends_the_search() {
        let token = CancellationToken::new();
        let stop = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_mins(10)).await;
            stop.cancel();
        });
        assert_eq!(detect_until_found(never_found, || {}, &token).await, None);
    }
}
