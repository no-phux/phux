//! Probe exchange, parsing, and late-reply settling.

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

/// DA1 ends the exchange wherever it lands; an echoed query or a cut-off
/// reply does not.
#[test]
fn saw_primary_da_needs_a_whole_reply() {
    assert!(saw_primary_da(
        b"\x1b]10;rgb:eded/eded/eded\x1b\\\x1b[?62;22c"
    ));
    assert!(!saw_primary_da(b"\x1b]10;rgb:eded/eded/eded\x1b\\"));
    assert!(!saw_primary_da(b"\x1b[c"));
    assert!(!saw_primary_da(b"\x1b[?62;22"));
}

/// A slow terminal's replies are read off the tty up to DA1, so none are
/// left for the shell, and the drain stops there rather than at the
/// deadline.
#[test]
fn drain_swallows_late_replies_through_da1() {
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        writer
            .write_all(b"\x1b]11;rgb:0a0a/0a0a/0a0a\x1b\\\x1b[?62;22c")
            .expect("write late replies");
        writer
    });
    let started = Instant::now();
    let mut response = b"\x1b]10;rgb:eded/eded/eded\x1b\\".to_vec();
    drain_until_da1(&reader, &mut response, started + Duration::from_secs(5));
    assert!(saw_primary_da(&response));
    assert!(started.elapsed() < Duration::from_secs(4), "stopped at DA1");
    drop(late.join().expect("writer thread"));
}
