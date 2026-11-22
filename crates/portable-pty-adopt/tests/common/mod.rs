//! Shared helpers for the PTY adoption tests.

#![allow(unreachable_pub, reason = "tests/common shared-helpers pattern")]

use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Ceiling for "the adopted child should have echoed by now" waits. Sized to
/// be unreachable: the assertion is that output appears, not how fast.
pub const OUTPUT_DEADLINE: Duration = Duration::from_secs(30);

/// Read `reader` on a thread until the accumulated output contains `needle`
/// or `timeout` passes.
pub fn read_until<R: std::io::Read + Send + 'static>(
    mut reader: R,
    needle: &str,
    timeout: Duration,
) -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut acc = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    acc.extend_from_slice(&buf[..n]);
                    if tx.send(String::from_utf8_lossy(&acc).into_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(seen) if seen.contains(needle) => return true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    false
}
