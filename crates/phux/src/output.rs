//! Stdout for the `phux` binary: the one place that handles a reader
//! hanging up (`phux snapshot work | head`). `println!` panics on `EPIPE`;
//! every stdout write here goes through `outln!`, `out!`, or `bytes`, and the
//! crate carries no `clippy::print_stdout` allow, so a stray `println!` fails
//! lint. Stderr stays on `eprintln!`.

use std::fmt;
use std::io::{self, ErrorKind, Write};
use std::process::{self, ExitCode};

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

/// Write raw bytes to stdout (payloads rendered elsewhere).
pub(crate) fn bytes(buf: &[u8]) {
    let mut out = io::stdout().lock();
    settle(out.write_all(buf));
}

/// Write raw bytes to stdout and flush now: `phux play` feeds a PTY, and a
/// line-buffered partial line (a prompt) would paint late.
pub(crate) fn bytes_now(buf: &[u8]) {
    // One lock for the write and the flush: a second `stdout().lock()` would
    // be a second chance for another writer to interleave between a chunk
    // and the flush that reveals it.
    let mut out = io::stdout().lock();
    settle(out.write_all(buf).and_then(|()| out.flush()));
}

/// Turn a stdout write result into the process outcome: a closed reader
/// exits 0, anything else is one stderr line and a failing status.
fn settle(result: io::Result<()>) {
    let Err(err) = result else { return };
    if err.kind() == ErrorKind::BrokenPipe {
        give_up();
    }
    // A real failure (full disk, `EIO`): one stderr line and a failing status.
    eprintln!("phux: cannot write to stdout: {err}");
    process::exit(EXIT_WRITE_FAILED);
}

/// End the process because stdout's reader is gone.
///
/// Exits here rather than returning a sentinel: every write site reports work
/// that already completed, so there is nothing to unwind, and threading a
/// `Result` through every renderer would reintroduce the panic at the first
/// missed `?`. Exit 0 because `| head` hanging up is the intended end. The cost
/// is that no destructors run (a `PHUX_LOG` tee may lose its tail).
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

/// `print!` that treats a closed reader as a clean exit. Line-buffered, like
/// `print!`.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::output::fragment(::core::format_args!($($arg)*))
    };
}

/// Print `value` as one pretty-printed JSON document. phux's own documents
/// always serialize, so a failure is reported as a bug on the `--json` error
/// contract.
pub(crate) fn json(value: &impl serde::Serialize) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::JSON_SERIALIZE,
                format!("could not render JSON: {err}"),
                "this is a phux bug; run `phux doctor` and report it",
            ),
            1,
        ),
    }
}
