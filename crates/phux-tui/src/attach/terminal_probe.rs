//! Best-effort probes of the cooked outer terminal.
//!
//! This runs before the attach driver enters raw mode. Failures are deliberately
//! silent to callers: palette discovery improves terminal fidelity but must
//! never make an otherwise-valid attach fail.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use phux_protocol::caps::{TerminalColor, TerminalDefaultColors};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use rustix::termios::{LocalModes, OptionalActions, SpecialCodeIndex, Termios};

const COLOR_QUERY: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\";

/// Total wall-clock budget for the whole OSC 10/11 exchange.
///
/// This sits on the attach critical path: nothing is sent to the server until
/// the probe answers, so its cost is added to every `attach_handshake`. A
/// terminal that answers at all answers in about a millisecond, even over
/// ssh; one that does not answer must not cost more than a frame. The old
/// `VMIN=0`/`VTIME=1` shape charged 100 ms per read and up to three reads.
const PROBE_BUDGET: Duration = Duration::from_millis(25);

/// Cap on bytes read while looking for the two OSC replies.
///
/// The tty is shared with the user, so a keystroke or a paste landing during
/// the probe is read here too. Reply pairs are well under 100 bytes.
const MAX_RESPONSE_BYTES: usize = 1024;

/// Probe OSC 10/11 on `/dev/tty`, returning `None` on any unsupported or
/// non-interactive path.
pub(super) fn default_colors() -> Option<TerminalDefaultColors> {
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let original = rustix::termios::tcgetattr(tty.as_fd()).ok()?;
    let _restore = TermiosRestore {
        tty: tty.try_clone().ok()?,
        original: original.clone(),
    };

    // Non-canonical, fully non-blocking reads: `poll` owns the waiting, so a
    // read never parks on the terminal's own timer. Keep signal handling
    // enabled; the probe must not alter Ctrl-C behavior.
    let mut probe_mode = original;
    probe_mode
        .local_modes
        .remove(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ECHONL);
    probe_mode.special_codes[SpecialCodeIndex::VMIN] = 0;
    probe_mode.special_codes[SpecialCodeIndex::VTIME] = 0;
    rustix::termios::tcsetattr(tty.as_fd(), OptionalActions::Now, &probe_mode).ok()?;

    tty.write_all(COLOR_QUERY).ok()?;
    tty.flush().ok()?;

    let deadline = Instant::now() + PROBE_BUDGET;
    let mut response = Vec::with_capacity(128);
    let mut chunk = [0_u8; 128];
    loop {
        if !wait_readable(&tty, deadline)? {
            return None;
        }
        let n = tty.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        response.extend_from_slice(&chunk[..n]);
        let (foreground, background) = parse_responses(&response);
        if let (Some(foreground), Some(background)) = (foreground, background) {
            return Some(TerminalDefaultColors {
                foreground,
                background,
            });
        }
        if response.len() >= MAX_RESPONSE_BYTES {
            return None;
        }
    }
}

/// Block until `tty` has input or `deadline` passes.
///
/// `Some(true)` means readable, `Some(false)` means the budget is spent, and
/// `None` means the probe cannot continue at all. An interrupted wait retries
/// against the same deadline so a signal cannot extend the budget.
fn wait_readable<Fd: AsFd>(tty: &Fd, deadline: Instant) -> Option<bool> {
    wait_with(deadline, Instant::now, |timeout| {
        // Rustix 1 takes a precise `Timespec`; retain the old round-up so a
        // sub-millisecond tail still waits rather than spinning to deadline.
        let timeout = Timespec::try_from(timeout).map_err(|_| Errno::INVAL)?;
        let mut fds = [PollFd::new(tty, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => Ok(false),
            Ok(_) => Ok(true),
            Err(e) => Err(e),
        }
    })
}

/// One deadline, one remaining-time poll, EINTR retries the same Instant.
///
/// The host-wall clock is not an oracle here: a descheduled test thread
/// can outlive `PROBE_BUDGET * 4` without the probe restarting its budget
/// (phux-tur1). Tests drive this with a fake clock so contention cannot
/// be mistaken for a restarted deadline.
fn wait_with(
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut poll_once: impl FnMut(Duration) -> Result<bool, Errno>,
) -> Option<bool> {
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Some(false);
        }
        match poll_once(remaining.max(Duration::from_millis(1))) {
            Ok(false) => return Some(false),
            Ok(true) => return Some(true),
            Err(Errno::INTR) => {}
            Err(_) => return None,
        }
    }
}

struct TermiosRestore {
    tty: File,
    original: Termios,
}

impl Drop for TermiosRestore {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(self.tty.as_fd(), OptionalActions::Now, &self.original);
    }
}

fn parse_responses(bytes: &[u8]) -> (Option<TerminalColor>, Option<TerminalColor>) {
    let mut foreground = None;
    let mut background = None;
    let mut cursor = 0;
    while cursor + 2 <= bytes.len() {
        let Some(start) = bytes[cursor..].windows(2).position(|w| w == b"\x1b]") else {
            break;
        };
        let payload_start = cursor + start + 2;
        let Some((payload_end, terminator_len)) = osc_end(&bytes[payload_start..]) else {
            break;
        };
        let payload_end = payload_start + payload_end;
        if let Some((selector, value)) = split_once(&bytes[payload_start..payload_end], b';')
            && let Ok(selector) = std::str::from_utf8(selector)
            && let Ok(selector) = selector.parse::<u8>()
            && let Some(color) = parse_color(value)
        {
            match selector {
                10 => foreground = Some(color),
                11 => background = Some(color),
                _ => {}
            }
        }
        cursor = payload_end + terminator_len;
    }
    (foreground, background)
}

fn osc_end(bytes: &[u8]) -> Option<(usize, usize)> {
    for (idx, byte) in bytes.iter().enumerate() {
        if *byte == b'\x07' {
            return Some((idx, 1));
        }
        if *byte == b'\x1b' && bytes.get(idx + 1) == Some(&b'\\') {
            return Some((idx, 2));
        }
    }
    None
}

fn split_once(bytes: &[u8], delimiter: u8) -> Option<(&[u8], &[u8])> {
    let idx = bytes.iter().position(|byte| *byte == delimiter)?;
    Some((&bytes[..idx], &bytes[idx + 1..]))
}

fn parse_color(value: &[u8]) -> Option<TerminalColor> {
    let value = std::str::from_utf8(value).ok()?;
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let mut components = rgb.split('/');
        let r = normalize_component(components.next()?)?;
        let g = normalize_component(components.next()?)?;
        let b = normalize_component(components.next()?)?;
        return components
            .next()
            .is_none()
            .then_some(TerminalColor { r, g, b });
    }
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some(TerminalColor {
        r: u8::from_str_radix(&hex[0..2], 16).ok()?,
        g: u8::from_str_radix(&hex[2..4], 16).ok()?,
        b: u8::from_str_radix(&hex[4..6], 16).ok()?,
    })
}

fn normalize_component(component: &str) -> Option<u8> {
    if component.is_empty() || component.len() > 4 {
        return None;
    }
    let value = u16::from_str_radix(component, 16).ok()?;
    let max = (1_u32 << (component.len() * 4)) - 1;
    u8::try_from((u32::from(value) * 255 + max / 2) / max).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn parses_fragment_ready_x11_and_hash_responses() {
        let bytes = b"noise\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\\x1b]11;#12181b\x07";
        let (foreground, background) = parse_responses(bytes);
        assert_eq!(
            foreground,
            Some(TerminalColor {
                r: 208,
                g: 208,
                b: 208
            })
        );
        assert_eq!(
            background,
            Some(TerminalColor {
                r: 18,
                g: 24,
                b: 27
            })
        );
    }

    #[test]
    fn incomplete_response_is_ignored() {
        assert_eq!(parse_responses(b"\x1b]10;rgb:ff/ff/ff"), (None, None));
    }

    /// A silent source costs one remaining-time poll, not a restarted budget.
    ///
    /// The clock and `poll` are fakes so host descheduling cannot be judged
    /// as probe work (phux-tur1). A restarted deadline would poll again
    /// with a fresh 20 ms after the first timeout.
    #[test]
    fn silent_source_gives_up_at_the_deadline() {
        let start = Instant::now();
        let budget = Duration::from_millis(20);
        let deadline = start + budget;
        let now = Cell::new(start);
        let mut polls = Vec::new();
        let waited = wait_with(
            deadline,
            || now.get(),
            |timeout| {
                assert!(
                    polls.len() < 2,
                    "a timed-out poll must not restart the deadline"
                );
                polls.push(timeout);
                now.set(now.get() + timeout);
                Ok(false)
            },
        );
        assert_eq!(waited, Some(false));
        assert_eq!(polls, [budget]);
        assert!(now.get() <= deadline + Duration::from_millis(1));
    }

    /// `EINTR` retries the same Instant; it does not extend the budget.
    #[test]
    fn interrupted_wait_retries_the_same_deadline() {
        let start = Instant::now();
        let deadline = start + Duration::from_millis(20);
        let now = Cell::new(start);
        let mut polls = Vec::new();
        let waited = wait_with(
            deadline,
            || now.get(),
            |timeout| {
                polls.push(timeout);
                if polls.len() == 1 {
                    now.set(now.get() + Duration::from_millis(5));
                    Err(Errno::INTR)
                } else {
                    now.set(now.get() + timeout);
                    Ok(false)
                }
            },
        );
        assert_eq!(waited, Some(false));
        assert_eq!(
            polls,
            [Duration::from_millis(20), Duration::from_millis(15)]
        );
    }

    /// The rustix wrapper times out on a real silent pipe. No host-wall
    /// ceiling: that was the phux-tur1 flake. Hang detection is nextest's.
    #[test]
    fn silent_pipe_times_out_through_poll() {
        let (reader, _writer) = std::io::pipe().expect("pipe");
        assert_eq!(
            wait_readable(&reader, Instant::now() + Duration::from_millis(20)),
            Some(false)
        );
    }

    /// A terminal that answers is not made to wait out the budget.
    #[test]
    fn readable_source_returns_before_the_deadline() {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(b"answer").expect("write answer");
        drop(writer);
        let waited = wait_readable(&reader, Instant::now() + Duration::from_secs(5));
        assert_eq!(waited, Some(true));
    }

    /// An expired deadline never enters `poll`.
    #[test]
    fn spent_budget_does_not_wait() {
        let start = Instant::now();
        let mut polls = 0_u8;
        let waited = wait_with(
            start,
            || start,
            |_| {
                polls += 1;
                Ok(true)
            },
        );
        assert_eq!(waited, Some(false));
        assert_eq!(polls, 0);
    }
}
