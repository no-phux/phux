//! Waiting for a key's echo inside the frame that sent it.
//!
//! Rune runs a key inside a frame and presents that frame. An echo landing
//! a moment later finds a surface that just presented, so it waits for the
//! display's next opportunity: a refresh or more (16.7 ms at 60 Hz). A local
//! server answers in about half a millisecond, so the key's frame holds for
//! the view's next publication, up to [`ECHO_WAIT`], and draws the echo
//! itself. The runtime's publish wakes the wait; nothing polls. `PHUX_ECHO_FAST=0` turns this off for a before/after.

use std::sync::Arc;
use std::time::Duration;

use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client, ViewId};

/// The longest a key's frame waits. A local server's echo is back within it
/// 99 times in 100 (`scripts/bench/rune-echo.sh`), and it stays under the
/// 8 ms Rune calls a stall.
pub(super) const ECHO_WAIT: Duration = Duration::from_millis(4);

/// Return once `view` publishes a frame newer than `before` or goes away,
/// or after [`ECHO_WAIT`]. A key the program doesn't echo (a password
/// prompt) pays the whole wait.
pub(super) fn await_echo(client: &Client, view: ViewId, before: &Arc<GridFrame>) {
    if !phux_perf::echo_hop::fast_path_enabled() {
        return;
    }
    if let Some(slot) = client.view_slot(view) {
        let _ = slot.wait_past(before.generation, ECHO_WAIT);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use phux_protocol::ids::{BootstrapId, StreamId};
    use phux_protocol::wire::frame::FrameKind;

    use super::{ECHO_WAIT, await_echo};

    #[test]
    fn an_echo_ends_the_wait_and_silence_is_bounded() {
        let (client, rt) = crate::grid::budget::input_fixture();
        let shown = rt.state.shown.as_ref().unwrap();
        let (terminal_id, view) = (shown.resource.clone(), shown.view.unwrap());
        let before = client.acquire_view(view).unwrap();
        let start = Instant::now();
        await_echo(&client, view, &before);
        let silent = start.elapsed();
        assert!(silent >= ECHO_WAIT && silent < ECHO_WAIT + Duration::from_millis(50));
        // An echo published mid-wait wakes it.
        let server = Arc::clone(&client);
        let echo = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1));
            server
                .feed(FrameKind::ResourceOutput {
                    terminal_id,
                    stream_id: StreamId::new(1).unwrap(),
                    bootstrap_id: BootstrapId::new(1).unwrap(),
                    seq: 1,
                    bytes: b"x".to_vec().into(),
                })
                .unwrap();
        });
        let start = Instant::now();
        await_echo(&client, view, &before);
        assert!(start.elapsed() < ECHO_WAIT);
        echo.join().unwrap();
        assert!(client.acquire_view(view).unwrap().text().ends_with('x'));
    }
}
