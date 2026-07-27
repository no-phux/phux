//! Stdout for the `phux` binary — the one place that knows what to do when
//! the reader hangs up.
//!
//! `println!` panics when the write fails, and the write fails the moment a
//! human does an ordinary thing: `phux snapshot work | head -8`, or quitting
//! `less` before the listing ends. The reader closes its end, the next write
//! gets `EPIPE` (Rust masks `SIGPIPE` at startup, so it surfaces as an error
//! and not a signal), and `println!`'s internal `expect` unwinds. Before
//! this module existed that produced, for a perfectly healthy server, a
//! ~50-frame backtrace and an ERROR line reading `server panic` from the
//! process-global panic hook — a triage hazard on top of a crash on top of
//! a non-event (phux-h5hj.8, phux-ngq2).
//!
//! Every stdout write in this crate goes through `outln!`, `out!`, or
//! `bytes`. That is not a convention anyone has to remember: the bin crate
//! no longer carries a `clippy::print_stdout` allow, so a new `println!`
//! fails `just ci` instead of failing a user six months from now.
//!
//! Stderr is deliberately left on `eprintln!`. It is the diagnostic channel,
//! it is almost never the piped one, and routing it through here would mean
//! a failure to report a failure exits the process from inside the reporter.

use std::fmt;
use std::io::{self, ErrorKind, Write};
use std::process;

/// Status handed to the OS when a write fails for a reason that is *not* the
/// reader leaving. Mirrors `ExitCode::FAILURE`, which these paths cannot
/// return — see `give_up` for why they exit instead of propagating.
const EXIT_WRITE_FAILED: i32 = 1;

/// Write `args` to stdout, then a newline. Backs `outln!`.
pub(crate) fn line(args: fmt::Arguments<'_>) {
    // One lock for the payload and its newline so a line can never be split
    // by another writer. The verbs are single-threaded today; taking the
    // lock explicitly costs nothing and removes the question.
    let mut out = io::stdout().lock();
    settle(out.write_fmt(args).and_then(|()| out.write_all(b"\n")));
}

/// Write `args` to stdout with no trailing newline. Backs `out!`.
pub(crate) fn fragment(args: fmt::Arguments<'_>) {
    let mut out = io::stdout().lock();
    settle(out.write_fmt(args));
}

/// Write raw bytes to stdout.
///
/// For payloads rendered into a buffer by someone else's API — `phux
/// completion <shell>`, whose script comes out of `clap_complete` — rather
/// than formatted here.
pub(crate) fn bytes(buf: &[u8]) {
    let mut out = io::stdout().lock();
    settle(out.write_all(buf));
}

/// Turn the result of a stdout write into the right process outcome.
fn settle(result: io::Result<()>) {
    let Err(err) = result else { return };
    if err.kind() == ErrorKind::BrokenPipe {
        give_up();
    }
    // Anything else is a real failure the user wants to know about: a full
    // disk under `phux snapshot --json > out.json`, `EIO` on a dying tty.
    // One stderr line and a failing status says what the old panic said,
    // without 50 frames of noise and without exit code 101.
    eprintln!("phux: cannot write to stdout: {err}");
    process::exit(EXIT_WRITE_FAILED);
}

/// End the process because stdout's reader is gone.
///
/// DESIGN — why this exits here instead of returning a sentinel that each
/// verb propagates back to `main`:
///
/// 1. A sentinel would put the contract straight back into the caller's
///    head, which is the thing this module exists to delete. The write
///    sites sit inside `-> ()` renderers (`print_screen_box`, the `doctor`
///    summary, the `service install` report); every one of them, plus every
///    frame between them and `main`, would have to grow a `Result` and
///    remember to forward it. One missed `?` and the panic is back —
///    silently, in whichever verb nobody piped into `head` until a user did.
/// 2. There is nothing left to unwind. Every stdout write in this crate
///    *reports work that already completed*: the cast file is finalized
///    before `rec` prints its summary, the unit file is on disk before
///    `service install` prints it, the session is already dead before
///    `kill` says so. No caller is mid-mutation at a write site, so an
///    unwind would buy nothing but another chance to write a report to a
///    reader that left.
/// 3. Zero, not 141 or 101: a reader that hung up is the intended end of
///    `| head`, not a failure. The shell reports the last pipeline element's
///    status anyway, and a nonzero here would trip `set -o pipefail` scripts
///    over a non-event.
///
/// The cost, stated plainly: `process::exit` runs no destructors. A
/// `PHUX_LOG` file tee can lose the tail buffered in its non-blocking
/// writer, and a `--features dhat-heap` build will not write its
/// `dhat-heap.json` — both are diagnostics, both only for the invocation
/// whose reader walked away, and neither is a build a user runs. The
/// client's detach path already accepts the same trade (see
/// `phux_server::telemetry::init_client`); nothing else a one-shot verb
/// holds has a `Drop` that matters. Flushing stdout first is pointless: the
/// pipe it would flush into is the thing that just went away.
fn give_up() -> ! {
    process::exit(0)
}

/// `println!` that treats a closed reader as a clean exit instead of a
/// panic. Identical formatting surface; use it everywhere `println!` would
/// have gone.
macro_rules! outln {
    () => {
        $crate::output::line(::core::format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::output::line(::core::format_args!($($arg)*))
    };
}

/// `print!` that treats a closed reader as a clean exit instead of a panic.
///
/// Stdout stays line-buffered, so a fragment with no newline waits in the
/// buffer for one — exactly as with `print!`.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::output::fragment(::core::format_args!($($arg)*))
    };
}
