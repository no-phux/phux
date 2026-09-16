//! The confirmation every dangerous verb shares (ADR-0128): `kill`,
//! `signal interrupt|terminate|kill`, `detach`, and `approve` act only after
//! `--yes` or a "y" typed at a terminal prompt.
//!
//! What is dangerous is the catalog's fact
//! ([`phux_protocol::kinds::MethodSpec::dangerous`]), the same one MCP's
//! `destructiveHint` and `confirm` argument and `phux resource methods`
//! report. With stdin not a terminal and no `--yes` there is no one to ask,
//! so the verb refuses with exit 2 before it dials anything.

use std::io::{self, BufRead, IsTerminal};
use std::process::ExitCode;

use phux_protocol::wire::frame::TerminalSignal;

/// The exit code of an unconfirmed dangerous verb: nothing was sent.
pub(crate) const NOT_CONFIRMED: u8 = 2;

/// Whether the verb may go ahead: `--yes`, or a "y" at a terminal prompt.
///
/// # Errors
///
/// [`NOT_CONFIRMED`] when stdin is not a terminal and `yes` is unset, or
/// when the prompt's answer is anything but yes.
pub(crate) fn confirmed(yes: bool, action: &str) -> Result<(), ExitCode> {
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    consent(yes, interactive, action, || read_answer(&stdin))
}

/// [`confirmed`] with the terminal check and the answer injected.
pub(crate) fn consent(
    yes: bool,
    interactive: bool,
    action: &str,
    answer: impl FnOnce() -> Option<String>,
) -> Result<(), ExitCode> {
    if yes {
        return Ok(());
    }
    if !interactive {
        eprintln!(
            "phux: refusing to {action} without confirmation: pass --yes \
             (stdin is not a terminal, so there is no one to ask)"
        );
        return Err(ExitCode::from(NOT_CONFIRMED));
    }
    eprint!("phux: {action}? [y/N] ");
    if answer().is_some_and(|reply| is_yes(&reply)) {
        return Ok(());
    }
    eprintln!("phux: not confirmed; nothing was sent");
    Err(ExitCode::from(NOT_CONFIRMED))
}

fn is_yes(reply: &str) -> bool {
    matches!(reply.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn read_answer(stdin: &io::Stdin) -> Option<String> {
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line).ok()?;
    (read > 0).then_some(line)
}

/// Whether delivering `signal` is dangerous: the catalog's payload rule, so
/// `freeze` and `resume`, the reversible brake, never ask.
pub(crate) fn signal_is_dangerous(signal: TerminalSignal) -> bool {
    phux_client::signal::is_dangerous(signal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_or_a_typed_yes_confirms_and_nothing_else_does() {
        assert!(consent(true, false, "kill x", || None).is_ok());
        assert!(consent(false, true, "kill x", || Some("y\n".to_owned())).is_ok());
        assert!(consent(false, true, "kill x", || Some(" YES ".to_owned())).is_ok());
        for reply in [
            None,
            Some(String::new()),
            Some("n\n".to_owned()),
            Some("yep".to_owned()),
        ] {
            assert_eq!(
                consent(false, true, "kill x", || reply),
                Err(ExitCode::from(NOT_CONFIRMED))
            );
        }
        assert_eq!(
            consent(false, false, "kill x", || unreachable!("never asks")),
            Err(ExitCode::from(NOT_CONFIRMED))
        );
    }

    #[test]
    fn only_the_ending_signals_ask() {
        for signal in [
            TerminalSignal::Interrupt,
            TerminalSignal::Terminate,
            TerminalSignal::Kill,
        ] {
            assert!(signal_is_dangerous(signal), "{signal:?}");
        }
        for signal in [TerminalSignal::Freeze, TerminalSignal::Resume] {
            assert!(!signal_is_dangerous(signal), "{signal:?}");
        }
    }
}
