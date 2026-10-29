//! The confirmation every dangerous verb shares (ADR-0128): `kill`,
//! `signal interrupt|terminate|kill`, `detach`, and `approve` need `--yes` or a
//! terminal "y". Dangerous is the catalog's fact
//! ([`phux_protocol::kinds::MethodSpec::dangerous`]). Without a terminal and
//! `--yes` the verb refuses with exit 2 before dialing.

use std::io::{self, BufRead, IsTerminal};
use std::process::ExitCode;

/// The exit code of an unconfirmed dangerous verb: nothing was sent.
pub(crate) const NOT_CONFIRMED: u8 = 2;

/// Whether the verb may go ahead: `--yes`, or a "y" at a terminal prompt.
///
/// # Errors
///
/// [`NOT_CONFIRMED`] when stdin is not a terminal and `yes` is unset, or
/// when the prompt's answer is anything but yes.
pub(crate) fn confirmed(yes: bool, action: &str) -> Result<(), ExitCode> {
    confirmed_for(false, yes, action)
}

/// [`confirmed`] for a `--json` verb: a refusal is the `confirmation_required`
/// error document, not prose.
pub(crate) fn confirmed_for(json: bool, yes: bool, action: &str) -> Result<(), ExitCode> {
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    consent_for(json, yes, interactive, action, || read_answer(&stdin))
}

/// [`confirmed`] with the terminal check and the answer injected.
pub(crate) fn consent(
    yes: bool,
    interactive: bool,
    action: &str,
    answer: impl FnOnce() -> Option<String>,
) -> Result<(), ExitCode> {
    consent_for(false, yes, interactive, action, answer)
}

/// [`consent`] reporting a refusal as JSON under `json`.
pub(crate) fn consent_for(
    json: bool,
    yes: bool,
    interactive: bool,
    action: &str,
    answer: impl FnOnce() -> Option<String>,
) -> Result<(), ExitCode> {
    if yes {
        return Ok(());
    }
    if !interactive {
        return Err(refuse(
            json,
            format!(
                "refusing to {action} without confirmation: pass --yes \
                 (stdin is not a terminal, so there is no one to ask)"
            ),
        ));
    }
    // Interactive prompt deliberately has no diagnostic label or newline.
    eprint!("phux: {action}? [y/N] ");
    if answer().is_some_and(|reply| is_yes(&reply)) {
        return Ok(());
    }
    if json {
        return Err(refuse(json, format!("{action}: not confirmed")));
    }
    info!("not confirmed; nothing was sent");
    Err(ExitCode::from(NOT_CONFIRMED))
}

fn refuse(json: bool, message: String) -> ExitCode {
    use crate::commands::json_err::{CliError, codes, emit};
    emit(
        json,
        &CliError::new(
            codes::CONFIRMATION_REQUIRED,
            message,
            "nothing was sent; pass --yes to confirm",
        ),
        NOT_CONFIRMED,
    )
}

fn is_yes(reply: &str) -> bool {
    matches!(reply.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn read_answer(stdin: &io::Stdin) -> Option<String> {
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line).ok()?;
    (read > 0).then_some(line)
}

#[cfg(test)]
mod tests {
    use phux_client::signal::is_dangerous;
    use phux_protocol::wire::frame::TerminalSignal;

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

    /// Under `--json` an unconfirmed verb still exits 2 having sent nothing;
    /// only the channel changes. `--yes` never asks.
    #[test]
    fn a_json_refusal_keeps_the_exit_status() {
        assert_eq!(
            consent_for(true, false, false, "kill x", || unreachable!("never asks")),
            Err(ExitCode::from(NOT_CONFIRMED))
        );
        assert_eq!(
            consent_for(true, false, true, "kill x", || Some("n\n".to_owned())),
            Err(ExitCode::from(NOT_CONFIRMED))
        );
        assert!(consent_for(true, true, false, "kill x", || None).is_ok());
    }

    /// `freeze` and `resume`, the reversible brake, never ask.
    #[test]
    fn only_the_ending_signals_ask() {
        for signal in [
            TerminalSignal::Interrupt,
            TerminalSignal::Terminate,
            TerminalSignal::Kill,
        ] {
            assert!(is_dangerous(signal), "{signal:?}");
        }
        for signal in [TerminalSignal::Freeze, TerminalSignal::Resume] {
            assert!(!is_dangerous(signal), "{signal:?}");
        }
    }
}
