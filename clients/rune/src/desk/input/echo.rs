//! Waiting for a key's echo inside the frame that sent it.
//!
//! Rune runs a key inside a frame and presents that frame. An echo landing
//! a moment later finds a surface that just presented, so it waits for the
//! display's next opportunity: a refresh or more (16.7 ms at 60 Hz). A local
//! server answers in about half a millisecond, so the key's frame holds for
//! the runtime's next publication, up to [`ECHO_WAIT`], and draws the echo
//! itself. `PHUX_ECHO_FAST=0` turns this off for a before/after.

use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client, ViewId};

/// The longest a key's frame waits. A local server's echo is back within it
/// 99 times in 100 (`scripts/bench/rune-echo.sh`), and it stays under the
/// 8 ms Rune calls a stall.
pub(super) const ECHO_WAIT: Duration = Duration::from_millis(4);

/// Return once `view` publishes a frame other than `before`, or after
/// [`ECHO_WAIT`].
// ponytail: polls every 100 µs; a condvar on the runtime's publication if
// the wake-up slack ever shows in the trace. A key the program doesn't echo
// (a password prompt) pays the whole wait.
pub(super) fn await_echo(client: &Client, view: ViewId, before: &Arc<GridFrame>) {
    if !phux_perf::echo_hop::fast_path_enabled() {
        return;
    }
    let start = Instant::now();
    while start.elapsed() < ECHO_WAIT {
        if client
            .acquire_view(view)
            .is_none_or(|now| !Arc::ptr_eq(&now, before))
        {
            return;
        }
        std::thread::sleep(Duration::from_micros(100));
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{ECHO_WAIT, await_echo};

    #[test]
    fn an_echo_ends_the_wait_and_silence_is_bounded() {
        let (client, rt) = crate::grid::budget::input_fixture();
        let view = rt.state.shown.as_ref().and_then(|s| s.view).unwrap();
        let before = client.acquire_view(view).unwrap();
        let start = Instant::now();
        await_echo(&client, view, &before);
        let silent = start.elapsed();
        assert!(silent >= ECHO_WAIT && silent < ECHO_WAIT + Duration::from_millis(50));
        // A view showing anything but `before` returns at once.
        let (other, rt) = crate::grid::budget::input_fixture_showing("x");
        let elsewhere = other
            .acquire_view(rt.state.shown.as_ref().and_then(|s| s.view).unwrap())
            .unwrap();
        let start = Instant::now();
        await_echo(&client, view, &elsewhere);
        assert!(start.elapsed() < ECHO_WAIT);
    }
}
