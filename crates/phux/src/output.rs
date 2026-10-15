//! Stdout for the `phux` binary: the one place that handles a reader
//! hanging up (`phux snapshot work | head`). `println!` panics on `EPIPE`;
//! every stdout write here goes through `outln!`, `out!`, or `bytes`, and the
//! crate carries no `clippy::print_stdout` allow, so a stray `println!` fails
//! lint. Human diagnostics use explicit severity macros; raw stderr is reserved
//! for protocol documents, pass-through logs, prompts, and terminal control.

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
    diagnostic(
        Severity::Error,
        format_args!("cannot write to stdout: {err}"),
    );
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

/// Semantic human labels, never inferred from rendered messages.
#[derive(Clone, Copy)]
pub(crate) enum Severity {
    Error,
    Warn,
    Info,
    Ok,
}

impl Severity {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Ok => "ok",
        }
    }

    const fn style(self) -> anstyle::Style {
        use anstyle::AnsiColor;
        let color = match self {
            Self::Error => AnsiColor::Red,
            Self::Warn => AnsiColor::Yellow,
            Self::Info => AnsiColor::Cyan,
            Self::Ok => AnsiColor::Green,
        };
        anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(color)))
    }
}

/// One policy, evaluated for the stream being written (help/doctor stdout,
/// diagnostics stderr). Empty `NO_COLOR` does not disable color.
pub(crate) fn color_enabled(terminal: bool) -> bool {
    color_policy(
        terminal,
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("CLICOLOR_FORCE").as_deref(),
        std::env::var_os("CLICOLOR").as_deref(),
    )
}

fn color_policy(
    terminal: bool,
    no_color: Option<&std::ffi::OsStr>,
    term: Option<&std::ffi::OsStr>,
    force: Option<&std::ffi::OsStr>,
    clicolor: Option<&std::ffi::OsStr>,
) -> bool {
    if no_color.is_some_and(|v| !v.is_empty()) || term == Some(std::ffi::OsStr::new("dumb")) {
        return false;
    }
    if force.is_some_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    clicolor != Some(std::ffi::OsStr::new("0")) && terminal
}

/// Only the wordmark and severity carry styling; the actionable payload is plain.
pub(crate) fn human_line(severity: Severity, args: fmt::Arguments<'_>, color: bool) -> String {
    let label = severity.label();
    if color {
        let wordmark = anstyle::Style::new().bold();
        let style = severity.style();
        format!("{wordmark}phux{wordmark:#}: {style}{label}{style:#}: {args}")
    } else {
        format!("phux: {label}: {args}")
    }
}

pub(crate) fn diagnostic(severity: Severity, args: fmt::Arguments<'_>) {
    use std::io::IsTerminal as _;
    // Legacy prose errors can still be reached under --json. Keep those
    // machine-mode exceptions unstyled without changing their shape/status.
    let color = !crate::json_output_requested() && color_enabled(io::stderr().is_terminal());
    let line = human_line(severity, args, color);
    let _ = writeln!(io::stderr().lock(), "{line}");
}

/// Remedies may span lines, but every line is an unstyled hint.
pub(crate) fn hint(args: fmt::Arguments<'_>) {
    let mut stderr = io::stderr().lock();
    for line in args.to_string().lines() {
        let _ = writeln!(stderr, "  hint: {}", line.trim_start());
    }
}

macro_rules! error {
    ($($arg:tt)*) => { $crate::output::diagnostic($crate::output::Severity::Error, ::core::format_args!($($arg)*)) };
}
macro_rules! warn {
    ($($arg:tt)*) => { $crate::output::diagnostic($crate::output::Severity::Warn, ::core::format_args!($($arg)*)) };
}
macro_rules! info {
    ($($arg:tt)*) => { $crate::output::diagnostic($crate::output::Severity::Info, ::core::format_args!($($arg)*)) };
}
macro_rules! ok {
    ($($arg:tt)*) => { $crate::output::diagnostic($crate::output::Severity::Ok, ::core::format_args!($($arg)*)) };
}
macro_rules! hint {
    ($($arg:tt)*) => { $crate::output::hint(::core::format_args!($($arg)*)) };
}

/// Existing multi-line diagnostics have one failure followed by remedies.
/// The producers return payloads, not prefixed/rendered lines.
pub(crate) fn error_block<'a>(lines: impl IntoIterator<Item = &'a str>) {
    let mut lines = lines.into_iter();
    if let Some(message) = lines.next() {
        diagnostic(Severity::Error, format_args!("{message}"));
    }
    for remedy in lines {
        hint(format_args!("{remedy}"));
    }
}

#[cfg(test)]
mod tests {
    use super::{Severity, color_policy, human_line};
    use std::ffi::OsStr;

    #[test]
    fn color_policy_respects_precedence_and_stream_terminal() {
        let os = |s| Some(OsStr::new(s));
        assert!(!color_policy(false, None, None, None, None));
        assert!(color_policy(true, None, None, None, None));
        assert!(!color_policy(true, os("1"), None, os("1"), None));
        assert!(!color_policy(true, None, os("dumb"), os("1"), None));
        assert!(color_policy(false, os(""), None, os("1"), os("0")));
        assert!(!color_policy(true, None, None, os("0"), os("0")));
        assert!(!color_policy(false, None, None, os(""), None));
        assert!(!color_policy(true, None, None, None, os("0")));
    }

    #[test]
    fn human_line_styles_only_wordmark_and_label() {
        for (severity, label, color) in [
            (Severity::Error, "error", 31),
            (Severity::Warn, "warn", 33),
            (Severity::Info, "info", 36),
            (Severity::Ok, "ok", 32),
        ] {
            assert_eq!(
                human_line(severity, format_args!("message"), false),
                format!("phux: {label}: message")
            );
            assert_eq!(
                human_line(severity, format_args!("message"), true),
                format!("\x1b[1mphux\x1b[0m: \x1b[{color}m{label}\x1b[0m: message")
            );
        }
    }
}
