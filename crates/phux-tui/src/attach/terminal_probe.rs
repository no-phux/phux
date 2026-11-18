//! Best-effort probes of the cooked outer terminal before raw mode; failures
//! are silent. Sends OSC 10/11 then DA1: terminals answer DA1 in order even
//! when they ignore colour queries, so a bare DA1 ends the wait.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use phux_protocol::caps::{TerminalColor, TerminalDefaultColors};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use rustix::termios::{LocalModes, OptionalActions, SpecialCodeIndex, Termios};

/// OSC 10, OSC 11, then primary device attributes (`CSI c`): a DA1 reply
/// means missing colour replies are unsupported, not in flight.
const PROBE_QUERY: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c";

/// Wall-clock budget for the whole exchange, on the attach critical path;
/// spent only by a terminal that answers neither query.
const PROBE_BUDGET: Duration = Duration::from_millis(25);

/// Extra wait after the budget misses with no DA1, to swallow a late colour
/// reply that would otherwise print on the cooked terminal.
const LATE_REPLY_BUDGET: Duration = Duration::from_millis(40);

/// Cap on bytes read while looking for the replies (the tty is shared, so
/// typed input may land here too).
const MAX_RESPONSE_BYTES: usize = 1024;

/// How far one read of the probe got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeRead {
    /// Both OSC replies were parsed. Answering terminals take this path as
    /// soon as the colours are in hand, without waiting out DA1.
    Colors(TerminalDefaultColors),
    /// A primary-DA reply arrived and the two colour replies did not.
    Unsupported,
    /// The deadline passed with no decision.
    TimedOut,
    /// The tty could not be read. Callers may still spend the late budget.
    Failed,
}

/// Probe OSC 10/11 on `/dev/tty`; `None` on any unsupported or
/// non-interactive path. Returns as soon as both replies or a DA1 arrive.
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

    tty.write_all(PROBE_QUERY).ok()?;
    tty.flush().ok()?;

    let mut response = Vec::with_capacity(128);
    let first = read_replies(&tty, &mut response, Instant::now() + PROBE_BUDGET);
    let colors = palette_from_attempts(first, || {
        read_replies(&tty, &mut response, Instant::now() + LATE_REPLY_BUDGET)
    });
    discard_pending(&tty);
    colors
}

/// Colours from the first attempt. A DA1 "unsupported" answer does not spend
/// the late-reply budget; a timeout or a hard failure still does.
fn palette_from_attempts(
    first: ProbeRead,
    late: impl FnOnce() -> ProbeRead,
) -> Option<TerminalDefaultColors> {
    match first {
        ProbeRead::Colors(colors) => Some(colors),
        ProbeRead::Unsupported => None,
        ProbeRead::TimedOut | ProbeRead::Failed => match late() {
            ProbeRead::Colors(colors) => Some(colors),
            ProbeRead::Unsupported | ProbeRead::TimedOut | ProbeRead::Failed => None,
        },
    }
}

/// Read until the exchange is decided, `deadline` passes, or the tty fails.
fn read_replies(tty: &File, response: &mut Vec<u8>, deadline: Instant) -> ProbeRead {
    exchange(
        response,
        deadline,
        Instant::now,
        |timeout| poll_in(tty, timeout),
        |buf| rustix::io::read(tty, buf),
    )
}

/// One deadline, scripted reads. `poll_once` and `read_once` are split so
/// tests can answer DA1 without a real terminal.
fn exchange(
    response: &mut Vec<u8>,
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut poll_once: impl FnMut(Duration) -> Result<bool, Errno>,
    mut read_once: impl FnMut(&mut [u8]) -> Result<usize, Errno>,
) -> ProbeRead {
    let mut chunk = [0_u8; 128];
    loop {
        match classify(response) {
            ProbeProgress::Colors(colors) => return ProbeRead::Colors(colors),
            ProbeProgress::Unsupported => return ProbeRead::Unsupported,
            ProbeProgress::Pending => {}
        }
        if response.len() >= MAX_RESPONSE_BYTES {
            return ProbeRead::TimedOut;
        }
        match wait_with(deadline, &mut now, &mut poll_once) {
            Some(true) => {}
            Some(false) => return ProbeRead::TimedOut,
            None => return ProbeRead::Failed,
        }
        match read_once(&mut chunk) {
            Ok(0) => return ProbeRead::Failed,
            Ok(n) => response.extend_from_slice(&chunk[..n]),
            Err(Errno::INTR | Errno::AGAIN) => {}
            Err(_) => return ProbeRead::Failed,
        }
    }
}

/// What the bytes accumulated so far decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeProgress {
    Colors(TerminalDefaultColors),
    Unsupported,
    Pending,
}

/// Scan `bytes` in order: both colour replies decide success; a DA1 first,
/// or with only one colour, means the other is not coming.
fn classify(bytes: &[u8]) -> ProbeProgress {
    let mut foreground = None;
    let mut background = None;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != 0x1b {
            cursor += 1;
            continue;
        }
        match take_escape(&bytes[cursor..], &mut foreground, &mut background) {
            Take::NeedMore => return ProbeProgress::Pending,
            Take::Unsupported => return ProbeProgress::Unsupported,
            Take::Colors(colors) => return ProbeProgress::Colors(colors),
            Take::Advance(n) => cursor += n.max(1),
        }
    }
    ProbeProgress::Pending
}

enum Take {
    NeedMore,
    Unsupported,
    Colors(TerminalDefaultColors),
    /// How many bytes to consume. Callers treat 0 as 1 so a bad sequence
    /// cannot spin.
    Advance(usize),
}

fn take_escape(
    bytes: &[u8],
    foreground: &mut Option<TerminalColor>,
    background: &mut Option<TerminalColor>,
) -> Take {
    if bytes.len() < 2 {
        return Take::NeedMore;
    }
    match bytes[1] {
        b']' => take_osc(bytes, foreground, background),
        b'[' => take_csi(bytes),
        _ => Take::Advance(1),
    }
}

fn take_osc(
    bytes: &[u8],
    foreground: &mut Option<TerminalColor>,
    background: &mut Option<TerminalColor>,
) -> Take {
    let Some((payload_len, terminator_len)) = osc_end(&bytes[2..]) else {
        return Take::NeedMore;
    };
    note_osc(&bytes[2..2 + payload_len], foreground, background);
    if let Some(colors) = ready_colors(*foreground, *background) {
        return Take::Colors(colors);
    }
    Take::Advance(2 + payload_len + terminator_len)
}

fn take_csi(bytes: &[u8]) -> Take {
    match csi_body(&bytes[2..]) {
        CsiBody::Incomplete => Take::NeedMore,
        CsiBody::Invalid => Take::Advance(1),
        CsiBody::Final(final_idx) => {
            let body = &bytes[2..=2 + final_idx];
            if is_primary_da(body) {
                Take::Unsupported
            } else {
                Take::Advance(2 + final_idx + 1)
            }
        }
    }
}

enum CsiBody {
    Incomplete,
    Invalid,
    Final(usize),
}

/// `bytes` begins after `ESC [`. Parameter bytes are `0x30..=0x3f`,
/// intermediates `0x20..=0x2f`, and the final byte `0x40..=0x7e`.
fn csi_body(bytes: &[u8]) -> CsiBody {
    let mut intermediate = false;
    for (idx, byte) in bytes.iter().enumerate() {
        if (0x40..=0x7e).contains(byte) {
            return CsiBody::Final(idx);
        }
        if (0x20..=0x2f).contains(byte) {
            intermediate = true;
            continue;
        }
        if (0x30..=0x3f).contains(byte) && !intermediate {
            continue;
        }
        return CsiBody::Invalid;
    }
    CsiBody::Incomplete
}

/// Primary DA reply: `CSI ? Ps c`. The query itself (`CSI c` / `CSI 0 c`) is
/// not a reply, so an echoed query cannot end the probe.
fn is_primary_da(body: &[u8]) -> bool {
    let Some((&b'?', rest)) = body.split_first() else {
        return false;
    };
    let Some((&b'c', params)) = rest.split_last() else {
        return false;
    };
    params.iter().all(|byte| (0x30..=0x3f).contains(byte))
}

fn ready_colors(
    foreground: Option<TerminalColor>,
    background: Option<TerminalColor>,
) -> Option<TerminalDefaultColors> {
    Some(TerminalDefaultColors {
        foreground: foreground?,
        background: background?,
    })
}

/// Drop bytes already queued on `fd` without echoing them, restoring the
/// file-status flags (a leftover `NONBLOCK` breaks the shell's next read).
pub(super) fn discard_pending(fd: impl AsFd) {
    let fd = fd.as_fd();
    let Ok(flags) = rustix::fs::fcntl_getfl(fd) else {
        return;
    };
    if rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK).is_err() {
        return;
    }
    let mut buf = [0_u8; 256];
    loop {
        match rustix::io::read(fd, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let _ = rustix::fs::fcntl_setfl(fd, flags);
}

/// Block until `tty` has input (`Some(true)`) or `deadline` passes
/// (`Some(false)`); `None` when the probe cannot continue.
fn poll_in<Fd: AsFd>(fd: &Fd, timeout: Duration) -> Result<bool, Errno> {
    let timeout = Timespec::try_from(timeout).map_err(|_| Errno::INVAL)?;
    let mut fds = [PollFd::new(fd, PollFlags::IN)];
    match rustix::event::poll(&mut fds, Some(&timeout)) {
        Ok(0) => Ok(false),
        Ok(_) => Ok(true),
        Err(err) => Err(err),
    }
}

/// One deadline polled for its remaining time; EINTR retries the same
/// `Instant`. Tests drive it with a fake clock.
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
        // Swallow a reply that landed after the read loop gave up. Restoring
        // echo first prints it as `^[]10;rgb:...` on the cooked terminal.
        discard_pending(&self.tty);
        let _ = rustix::termios::tcsetattr(self.tty.as_fd(), OptionalActions::Now, &self.original);
    }
}

fn note_osc(
    payload: &[u8],
    foreground: &mut Option<TerminalColor>,
    background: &mut Option<TerminalColor>,
) {
    if let Some((selector, value)) = split_once(payload, b';')
        && let Ok(selector) = std::str::from_utf8(selector)
        && let Ok(selector) = selector.parse::<u8>()
        && let Some(color) = parse_color(value)
    {
        match selector {
            10 => *foreground = Some(color),
            11 => *background = Some(color),
            _ => {}
        }
    }
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
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::io::Read;

    struct Scripted {
        outcome: ProbeRead,
        elapsed: Duration,
        polls: Vec<Duration>,
    }

    fn sample_palette() -> TerminalDefaultColors {
        TerminalDefaultColors {
            foreground: TerminalColor {
                r: 208,
                g: 208,
                b: 208,
            },
            background: TerminalColor {
                r: 18,
                g: 24,
                b: 27,
            },
        }
    }

    /// Drive the exchange on a fake clock. A chunk is delivered on the next
    /// poll; an empty queue times out and advances the clock by that poll.
    fn drive(chunks: &[&[u8]], budget: Duration) -> Scripted {
        let start = Instant::now();
        let now = Cell::new(start);
        let chunks = RefCell::new(
            chunks
                .iter()
                .map(|chunk| chunk.to_vec())
                .collect::<VecDeque<_>>(),
        );
        let mut polls = Vec::new();
        let mut response = Vec::new();
        let outcome = exchange(
            &mut response,
            start + budget,
            || now.get(),
            |timeout| {
                polls.push(timeout);
                if chunks.borrow().is_empty() {
                    now.set(now.get() + timeout);
                    Ok(false)
                } else {
                    Ok(true)
                }
            },
            |buf| {
                let mut queue = chunks.borrow_mut();
                let Some(next) = queue.pop_front() else {
                    return Err(Errno::AGAIN);
                };
                let n = next.len().min(buf.len());
                buf[..n].copy_from_slice(&next[..n]);
                if n < next.len() {
                    queue.push_front(next[n..].to_vec());
                }
                Ok(n)
            },
        );
        Scripted {
            outcome,
            elapsed: now.get().saturating_duration_since(start),
            polls,
        }
    }

    #[test]
    fn parses_fragment_ready_x11_and_hash_responses() {
        let bytes = b"noise\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\\x1b]11;#12181b\x07";
        assert_eq!(classify(bytes), ProbeProgress::Colors(sample_palette()));
    }

    #[test]
    fn incomplete_response_is_ignored() {
        assert_eq!(classify(b"\x1b]10;rgb:ff/ff/ff"), ProbeProgress::Pending);
    }

    type ExchangeCase<'a> = (&'a str, &'a [&'a [u8]], ProbeRead, Option<usize>);

    /// How a reply ends the exchange early, without spending the budget. A
    /// terminal that ignores OSC 10/11 still answers DA1, and that reply ends
    /// it. Colours (with or without a trailing DA1) yield the palette as soon
    /// as both arrive. Replies arrive in order, so DA1 before both colours
    /// means the rest is unanswered. A short read inside `CSI ? Ps` and an
    /// echoed `CSI c` query are not yet a DA1 reply.
    #[test]
    fn a_reply_ends_the_exchange_before_the_deadline() {
        use ProbeRead::Unsupported;
        const FG: &[u8] = b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\";
        const BG: &[u8] = b"\x1b]11;#12181b\x07";
        const BOTH_DA1: &[u8] = b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\\x1b]11;#12181b\x07\x1b[?1;2c";
        const FG_DA1_BG: &[u8] = b"\x1b]10;rgb:ff/00/00\x1b\\\x1b[?6c\x1b]11;rgb:00/00/00\x1b\\";
        let colors = ProbeRead::Colors(sample_palette());
        #[rustfmt::skip] // a case table, one row per case
        let cases: [ExchangeCase<'_>; 6] = [
            ("da1 only", &[b"\x1b[?64;1;2;6;9;15;18;21;22c"], Unsupported, Some(1)),
            ("colours then da1 in one read", &[BOTH_DA1], colors, None),
            ("colours do not wait for da1", &[FG, BG, b"\x1b[?6c"], colors, Some(2)),
            ("da1 before both colours", &[FG_DA1_BG], Unsupported, None),
            ("partial da1", &[b"\x1b[?64;1;2", b"c"], Unsupported, Some(2)),
            ("echoed da1 query", &[b"\x1b[c", b"\x1b[?6c"], Unsupported, Some(2)),
        ];
        for (case, chunks, outcome, polls) in cases {
            let scripted = drive(chunks, Duration::from_millis(25));
            assert_eq!(scripted.outcome, outcome, "{case}");
            assert_eq!(scripted.elapsed, Duration::ZERO, "{case}");
            if let Some(polls) = polls {
                assert_eq!(scripted.polls.len(), polls, "{case}");
            }
        }
    }

    /// No bytes at all: one poll, and the clock advances by the budget.
    #[test]
    fn no_reply_hits_the_probe_budget() {
        let budget = Duration::from_millis(25);
        let scripted = drive(&[], budget);
        assert_eq!(scripted.outcome, ProbeRead::TimedOut);
        assert_eq!(scripted.elapsed, budget);
        assert_eq!(scripted.polls, [budget]);
    }

    /// DA1 (`Unsupported`) skips the late-reply attempt; a timeout still
    /// reads a late colour reply.
    #[test]
    fn only_a_timeout_spends_the_late_reply_budget() {
        let palette = Some(sample_palette());
        #[rustfmt::skip] // a case table, one row per case
        let cases = [
            ("unsupported da1 skips the late budget", ProbeRead::Unsupported, None, 0),
            ("a timeout reads a late colour reply", ProbeRead::TimedOut, palette, 1),
        ];
        for (case, first, want, attempts) in cases {
            let calls = Cell::new(0_u8);
            let colors = palette_from_attempts(first, || {
                calls.set(calls.get() + 1);
                want.map_or(ProbeRead::TimedOut, ProbeRead::Colors)
            });
            assert_eq!(colors, want, "{case}");
            assert_eq!(calls.get(), attempts, "{case}");
        }
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

    /// Queued OSC replies must not survive for the shell to echo.
    #[test]
    fn discard_pending_swallows_queued_bytes() {
        let (mut reader, mut writer) = std::io::pipe().expect("pipe");
        writer
            .write_all(b"\x1b]10;rgb:c5c5/d0d0/cdcd\x1b\\")
            .expect("write reply");
        drop(writer);
        discard_pending(&reader);
        let mut buf = [0_u8; 8];
        assert_eq!(reader.read(&mut buf).expect("read after discard"), 0);
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
