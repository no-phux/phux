//! Mapping a PTY child exit status to an [`ExitOutcome`].

#![allow(
    clippy::redundant_pub_crate,
    reason = "private submodule re-exported by spawn at crate visibility"
)]

use phux_core::process::ExitOutcome;

/// Map a `portable_pty::ExitStatus` to an [`ExitOutcome`].
///
/// Exit code, signal number, or neither when unknown. portable-pty reports signals by name
/// (`strsignal(3)` text, or `"signal N"` for adopted children), which
/// [`signal_number`] maps back.
pub(crate) fn exit_outcome(status: &portable_pty::ExitStatus) -> ExitOutcome {
    status.signal().map_or_else(
        || ExitOutcome::exited(i32::try_from(status.exit_code()).unwrap_or(i32::MAX)),
        |name| ExitOutcome {
            status: None,
            signal: signal_number(name),
        },
    )
}

/// The signal number behind a `portable_pty` signal name; `None` for a
/// non-signal name, never a guess.
fn signal_number(name: &str) -> Option<i32> {
    adopted_signal_number(name).or_else(|| {
        strsignal_names()
            .iter()
            .find(|(described, _)| described == name)
            .map(|(_, signal)| *signal)
    })
}

/// `portable-pty-adopt` renders a signal death as `"signal N"`.
fn adopted_signal_number(name: &str) -> Option<i32> {
    name.strip_prefix("signal ")?.parse().ok()
}

/// `portable_pty`'s name for each signal number, built by running its own
/// `ExitStatus` conversion over synthetic signal deaths, so names match this
/// platform exactly without a hand-kept table.
fn strsignal_names() -> &'static [(String, i32)] {
    use std::os::unix::process::ExitStatusExt;
    /// Covers the classic signals and Linux's real-time range.
    const HIGHEST_SIGNAL: i32 = 64;
    static NAMES: std::sync::OnceLock<Vec<(String, i32)>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        (1..=HIGHEST_SIGNAL)
            .filter_map(|signal| {
                // Low seven bits = the terminating signal (WIFSIGNALED).
                let status = std::process::ExitStatus::from_raw(signal);
                let converted = portable_pty::ExitStatus::from(status);
                converted.signal().map(|name| (name.to_owned(), signal))
            })
            .collect()
    })
}
