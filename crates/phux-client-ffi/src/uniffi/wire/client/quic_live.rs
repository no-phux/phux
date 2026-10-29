//! `RemoteClient` against a real server's loopback QUIC listener: the mobile
//! constructor's `quic://` string reaches ATTACHED through the shared runtime,
//! and an unpinned routable target fails closed before any socket opens.
//!
//! The servers live in [`servers`]; the cases are grouped by what they prove:
//! dialing, enrollment, event delivery, and the arrangement surface.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use std::thread;
use std::time::{Duration, Instant};

use super::*;

mod arrangement;
mod dial;
mod enroll;
mod events;
mod servers;

/// Generous, like the testkit's deadlines: a real server under a parallel run.
const DEADLINE: Duration = Duration::from_secs(20);
const SESSION: &str = "quic-live";

fn wait_for(remote: &RemoteClient, done: impl Fn(&RemoteClient) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done(remote) {
        assert!(
            Instant::now() < deadline,
            "timed out: status {:?}, last error {:?}",
            remote.status(),
            remote.last_error()
        );
        let _ = remote.take_events();
        thread::sleep(Duration::from_millis(20));
    }
}

/// Poll `take_publication` until `done` holds, keeping every event seen.
fn publish_until(
    remote: &RemoteClient,
    seen: &mut Vec<WireEvent>,
    done: impl Fn(&WirePublication) -> bool,
) -> WirePublication {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let publication = remote.take_publication();
        seen.extend(publication.events.iter().cloned());
        if done(&publication) {
            return publication;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {publication:?}, last error {:?}",
            remote.last_error()
        );
        thread::sleep(Duration::from_millis(20));
    }
}
