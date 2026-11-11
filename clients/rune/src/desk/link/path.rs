//! Notice when this computer moves to another network or wakes from sleep,
//! and tell the connection: a QUIC attach then migrates onto the new path
//! at once, where it would otherwise wait out its idle timeout.

use std::net::{IpAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime};

use rune::Tx;

use crate::desk::Desk;

/// How often the address is looked at while a remote is attached.
const EVERY: Duration = Duration::from_secs(3);
/// A timer may fire a little early; that reading still counts.
const SLACK: Duration = Duration::from_millis(250);
/// Wall time running this far ahead of the process's own clock means the
/// computer slept: the monotonic clock stops in sleep, the wall clock doesn't.
const SLEPT: Duration = Duration::from_secs(10);

/// The last address and clocks seen.
pub struct Path {
    addr: Option<IpAddr>,
    seen: Option<(Instant, SystemTime)>,
}

impl Default for Path {
    fn default() -> Self {
        Self {
            addr: local_addr(),
            seen: None,
        }
    }
}

/// The address this computer would send from. Connecting a UDP socket picks
/// the route and sends nothing; the peer is a documentation address.
fn local_addr() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

impl Path {
    /// Take a reading. True when the address changed to another one, or the
    /// computer slept since the last reading.
    fn moved(&mut self, addr: Option<IpAddr>, now: Instant, wall: SystemTime) -> bool {
        let slept = self.seen.is_some_and(|(then, then_wall)| {
            let ran = now.saturating_duration_since(then);
            wall.duration_since(then_wall)
                .is_ok_and(|passed| passed > ran + SLEPT)
        });
        // Losing the network is not a new path; coming back on one is.
        let changed = addr.is_some() && addr != self.addr;
        self.seen = Some((now, wall));
        if addr.is_some() {
            self.addr = addr;
        }
        changed || slept
    }

    fn due(&self, now: Instant) -> bool {
        self.seen
            .is_none_or(|(then, _)| now.saturating_duration_since(then) + SLACK >= EVERY)
    }
}

impl Desk {
    /// While this window is on another machine, watch the network path.
    pub(in crate::desk) fn follow_path(&mut self, tx: &mut Tx) {
        let (Some(client), Some(_)) = (&self.client, &self.machines.current) else {
            return;
        };
        let now = Instant::now();
        if !self.link.path.due(now) {
            return;
        }
        if self.link.path.moved(local_addr(), now, SystemTime::now()) {
            client.network_path_changed();
        }
        // One timer per reading: every poll asking would pile them up.
        tx.wake_after(EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_address_or_a_sleep_is_a_move_and_a_lost_network_is_not() {
        let ip = |last| Some(IpAddr::from([10, 0, 0, last]));
        let mut path = Path {
            addr: ip(1),
            seen: None,
        };
        let (start, wall) = (Instant::now(), SystemTime::now());
        let at = |secs| {
            (
                start + Duration::from_secs(secs),
                wall + Duration::from_secs(secs),
            )
        };
        let (now, clock) = at(0);
        assert!(!path.moved(ip(1), now, clock));
        let (now, clock) = at(3);
        assert!(!path.moved(None, now, clock), "offline is not a new path");
        let (now, clock) = at(6);
        assert!(!path.moved(ip(1), now, clock), "back on the same address");
        let (now, clock) = at(9);
        assert!(path.moved(ip(2), now, clock), "another network");
        // An hour of wall time across three seconds of running: it slept.
        let now = start + Duration::from_secs(12);
        assert!(path.moved(ip(2), now, wall + Duration::from_secs(3600)));
        assert!(!path.due(now + Duration::from_secs(1)));
        assert!(path.due(now + EVERY));
    }
}
