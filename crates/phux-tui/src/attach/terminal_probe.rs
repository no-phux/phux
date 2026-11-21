//! Best-effort probes of the cooked outer terminal before raw mode; failures
//! are silent. Sends OSC 10/11 then DA1: terminals answer DA1 in order even
//! when they ignore colour queries, so a bare DA1 ends the wait.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStringExt as _;
use std::sync::{Mutex, OnceLock};
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

/// Probe OSC 10/11 on `/dev/tty` once per process; `None` on any
/// unsupported or non-interactive path. Returns as soon as both replies or a
/// DA1 arrive. A retry or fallback attach reuses the answer: asking again
/// only puts more replies in flight.
pub(super) fn default_colors() -> Option<TerminalDefaultColors> {
    static PROBED: OnceLock<Option<TerminalDefaultColors>> = OnceLock::new();
    *PROBED.get_or_init(probe_once)
}

fn probe_once() -> Option<TerminalDefaultColors> {
    // `O_NOCTTY`: probing must never acquire a controlling terminal.
    let mut tty = File::from(
        rustix::fs::open(
            probe_device(),
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .ok()?,
    );
    let original = rustix::termios::tcgetattr(tty.as_fd()).ok()?;

    // Non-canonical, fully non-blocking reads: `poll` owns the waiting, so a
    // read never parks on the terminal's own timer. Keep signal handling
    // enabled; the probe must not alter Ctrl-C behavior.
    let mut probe_mode = original.clone();
    probe_mode
        .local_modes
        .remove(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ECHONL);
    probe_mode.special_codes[SpecialCodeIndex::VMIN] = 0;
    probe_mode.special_codes[SpecialCodeIndex::VTIME] = 0;
    rustix::termios::tcsetattr(tty.as_fd(), OptionalActions::Now, &probe_mode).ok()?;
    let mut exchange_state = InFlight {
        tty: tty.try_clone().ok()?,
        original,
        response: Vec::with_capacity(128),
        settle_by: Instant::now() + SETTLE_BUDGET,
    };

    tty.write_all(PROBE_QUERY).ok()?;
    tty.flush().ok()?;

    let InFlight { tty, response, .. } = &mut exchange_state;
    let first = read_replies(tty, response, Instant::now() + PROBE_BUDGET);
    let colors = palette_from_attempts(first, || {
        read_replies(tty, response, Instant::now() + LATE_REPLY_BUDGET)
    });
    if !saw_primary_da(&exchange_state.response) {
        // DA1 is the terminal's last reply; until it lands, echo stays off so
        // a slow terminal's answers cannot print on the cooked screen or
        // reach the shell. Raw mode or a failed attach settles it.
        *lock_in_flight() = Some(exchange_state);
    }
    colors
}

/// The terminal device to probe: stdin's (else stdout's) named tty. macOS
/// `poll` answers `POLLNVAL` at once for the `/dev/tty` alias, so probing
/// through it read nothing and let every reply leak; `/dev/tty` is the last
/// resort.
fn probe_device() -> std::path::PathBuf {
    let named = |fd: std::os::fd::BorrowedFd<'_>| {
        rustix::termios::isatty(fd)
            .then(|| rustix::termios::ttyname(fd, Vec::new()).ok())
            .flatten()
    };
    named(std::io::stdin().as_fd())
        .or_else(|| named(std::io::stdout().as_fd()))
        .map_or_else(
            || std::path::PathBuf::from("/dev/tty"),
            |name| std::ffi::OsString::from_vec(name.into_bytes()).into(),
        )
}

/// Wall-clock cap, from the query, on waiting out a slow terminal's replies
/// when an attach fails before raw mode.
const SETTLE_BUDGET: Duration = Duration::from_secs(1);

/// The probe's tty while its DA1 reply is still owed: probe mode (no echo)
/// until dropped, which discards queued replies and restores `original`.
struct InFlight {
    tty: File,
    original: Termios,
    response: Vec<u8>,
    settle_by: Instant,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        // Restoring echo first would print a queued reply as `^[]10;rgb:...`.
        discard_pending(&self.tty);
        let _ = rustix::termios::tcsetattr(self.tty.as_fd(), OptionalActions::Now, &self.original);
    }
}

static IN_FLIGHT: Mutex<Option<InFlight>> = Mutex::new(None);

fn lock_in_flight() -> std::sync::MutexGuard<'static, Option<InFlight>> {
    IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Hand the tty back from an unfinished probe. With `wait`, first swallow
/// the replies still owed (until DA1 or the settle budget): an attach that
/// fails on the cooked terminal calls this before the process exits, so the
/// replies never reach the shell. Raw mode calls it without waiting, as its
/// input loop absorbs late replies. A no-op when nothing is in flight.
pub(super) fn settle(wait: bool) {
    let Some(mut in_flight) = lock_in_flight().take() else {
        return;
    };
    if wait {
        let InFlight {
            tty,
            response,
            settle_by,
            ..
        } = &mut in_flight;
        drain_until_da1(tty, response, *settle_by);
    }
}

fn drain_until_da1(tty: &impl AsFd, response: &mut Vec<u8>, deadline: Instant) {
    let mut chunk = [0_u8; 128];
    while !saw_primary_da(response) && response.len() < MAX_RESPONSE_BYTES {
        if wait_with(deadline, Instant::now, |timeout| poll_in(tty, timeout)) != Some(true) {
            return;
        }
        match rustix::io::read(tty, &mut chunk) {
            Ok(0) => return,
            Ok(n) => response.extend_from_slice(&chunk[..n]),
            Err(Errno::INTR | Errno::AGAIN) => {}
            Err(_) => return,
        }
    }
}

/// Whether `bytes` holds the primary-DA reply that ends the exchange.
fn saw_primary_da(bytes: &[u8]) -> bool {
    (0..bytes.len()).any(|start| {
        let Some(after) = bytes[start..].strip_prefix(b"\x1b[") else {
            return false;
        };
        match csi_body(after) {
            CsiBody::Final(end) => is_primary_da(&after[..=end]),
            CsiBody::Incomplete | CsiBody::Invalid => false,
        }
    })
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
        // `POLLNVAL` / `POLLERR` with no input is not readiness: reading
        // would see EOF and end the probe before any reply.
        Ok(_) if !fds[0].revents().contains(PollFlags::IN) => Err(Errno::IO),
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
mod tests;
