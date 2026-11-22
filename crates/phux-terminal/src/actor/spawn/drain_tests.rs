use super::{ORPHAN_DRAIN_BUDGET, drain_master_with_budget};
use std::io::Read;

/// A reader that never runs out of bytes — the runaway child.
struct Endless;
impl Read for Endless {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
}

/// A reader that yields `n` full buffers and then reports EOF.
struct Finite(usize);
impl Read for Finite {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0 == 0 {
            return Ok(0);
        }
        self.0 -= 1;
        Ok(buf.len())
    }
}

/// A reader whose descriptor has gone away.
struct Broken;
impl Read for Broken {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from_raw_os_error(5)) // EIO
    }
}

#[test]
fn a_runaway_child_cannot_pin_the_reader_past_its_budget() {
    let mut buf = [0_u8; 64];
    let budget = std::time::Duration::from_millis(50);
    let started = std::time::Instant::now();
    drain_master_with_budget(&mut Endless, &mut buf, budget);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= budget,
        "the drain must actually serve its budget before giving up; gave up after {elapsed:?}",
    );
    assert!(
        elapsed < budget * 20,
        "the drain must give up NEAR its budget, not eventually; took {elapsed:?}",
    );
}

#[test]
fn eof_ends_the_drain_immediately() {
    let mut buf = [0_u8; 64];
    let started = std::time::Instant::now();
    drain_master_with_budget(&mut Finite(3), &mut buf, ORPHAN_DRAIN_BUDGET);
    assert!(
        started.elapsed() < ORPHAN_DRAIN_BUDGET,
        "a child that finishes must not hold the reader for the whole budget",
    );
}

#[test]
fn a_read_error_ends_the_drain_immediately() {
    let mut buf = [0_u8; 64];
    let started = std::time::Instant::now();
    drain_master_with_budget(&mut Broken, &mut buf, ORPHAN_DRAIN_BUDGET);
    assert!(
        started.elapsed() < ORPHAN_DRAIN_BUDGET,
        "EIO means the descriptor is gone; there is nothing left to drain",
    );
}
