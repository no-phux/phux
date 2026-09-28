//! Saying at the user's eye level that an answer is partial.
//!
//! A federation hub answers `GET_STATE` with a merged snapshot plus one
//! `ERROR` per unreachable satellite ([`phux_client::state::StateView`] keeps
//! them). Only terminals aggregate; session and window lists never do, so an
//! unreachable satellite cannot add or hide a session. Hence two rules:
//!
//! - Verbs that read (`phux ls`): the partial answer stands. Warn on stderr
//!   (and in the `--json` `unreachable` list) and exit 0.
//! - Verbs that resolve a Terminal target (`kill`, `tag`, `agent set`, ...): a
//!   miss against a partial view may be a pane on the unseen side, so it gets
//!   its own sentence and [`EXIT_PARTIAL_VIEW`], never "no such target". A hit
//!   still warns, since a set-valued selector may have matched a subset.

use std::process::ExitCode;

use phux_client::state::Degradation;

use crate::commands::json_err::{self, CliError, codes};

/// Exit status 3: no answer, because the fleet view was incomplete. Retrying
/// once the satellite is back is right for 3 and wrong for 1.
pub(crate) use crate::exit_codes::EXIT_PARTIAL_VIEW;

/// Warn once per unreachable satellite that `verb` acted on a partial view;
/// a no-op for a complete view.
pub(crate) fn warn_partial_view(verb: &str, degradation: &Degradation) {
    for notice in degradation.notices() {
        eprintln!("{}", phux_client::state::partial_view_warning(verb, notice));
    }
}

/// Report a selector that matched nothing, distinguishing the two causes,
/// and return [`EXIT_PARTIAL_VIEW`] or `1`. `target` is `None` for the
/// focused-pane default.
pub(crate) fn report_target_miss(target: Option<&str>, degradation: &Degradation) -> ExitCode {
    report_miss(target, degradation, ExitCode::from(EXIT_PARTIAL_VIEW))
}

/// As [`report_target_miss`] but always exiting `1`, for verbs whose exit
/// space is taken (`run` mirrors the child's code, `wait` owns 124). The
/// sentence still refuses to call the target absent.
pub(crate) fn report_target_miss_keeping_status(
    target: Option<&str>,
    degradation: &Degradation,
) -> ExitCode {
    report_miss(target, degradation, ExitCode::FAILURE)
}

/// Json-aware [`report_target_miss`]: `no_such_target`/1 for a whole fleet,
/// `partial_view`/[`EXIT_PARTIAL_VIEW`] for a partial one.
pub(crate) fn report_target_miss_for(
    json: bool,
    target: Option<&str>,
    degradation: &Degradation,
) -> ExitCode {
    if !json {
        return report_target_miss(target, degradation);
    }
    let (err, exit_code) = miss_error(target, degradation, EXIT_PARTIAL_VIEW);
    json_err::emit(true, &err, exit_code)
}

/// Json-aware [`report_target_miss_keeping_status`]: the document still says
/// `partial_view`, the status stays `1`.
pub(crate) fn report_target_miss_keeping_status_for(
    json: bool,
    target: Option<&str>,
    degradation: &Degradation,
) -> ExitCode {
    if !json {
        return report_target_miss_keeping_status(target, degradation);
    }
    let (err, exit_code) = miss_error(target, degradation, 1);
    json_err::emit(true, &err, exit_code)
}

/// The [`CliError`] and exit code for a selector miss; `degraded_status` is
/// the partial-view status.
fn miss_error(
    target: Option<&str>,
    degradation: &Degradation,
    degraded_status: u8,
) -> (CliError, u8) {
    if degradation.is_complete() {
        let message = target.map_or_else(
            || "no such target".to_owned(),
            |target| format!("no such target: {target}"),
        );
        return (
            CliError::new(
                codes::NO_SUCH_TARGET,
                message,
                "run `phux ls` to see live sessions and panes",
            ),
            1,
        );
    }
    // Deliberately never the words "no such target": this client does not
    // know that (see the module docs).
    let message = target.map_or_else(
        || {
            "could not resolve the target: this server's view of the fleet is \
             incomplete, so a miss here does not mean the target is gone"
                .to_owned()
        },
        |target| {
            format!(
                "could not resolve '{target}': this server's view of the fleet is \
                 incomplete, so a miss here does not mean the target is gone"
            )
        },
    );
    let unreachable = degradation
        .notices()
        .iter()
        .map(|notice| format!("unreachable — {notice}"))
        .collect::<Vec<_>>()
        .join("; ");
    (
        CliError::new(
            codes::PARTIAL_VIEW,
            message,
            format!("retry once the satellite link is back; {unreachable}"),
        ),
        degraded_status,
    )
}

/// Shared body: the wording is identical; only the degraded status differs.
fn report_miss(
    target: Option<&str>,
    degradation: &Degradation,
    degraded_status: ExitCode,
) -> ExitCode {
    if degradation.is_complete() {
        // Unchanged from before this module: a complete view that contains no
        // match is a real miss, and the wording scripts already grep for.
        match target {
            Some(target) => eprintln!("phux: no such target: {target}"),
            None => eprintln!("phux: no such target"),
        }
        return ExitCode::FAILURE;
    }
    // Deliberately never the word "no such target": this client does not know
    // that, and the sentence it would print is the one a user acts on.
    match target {
        Some(target) => eprintln!(
            "phux: could not resolve '{target}': this server's view of the fleet is \
             incomplete, so a miss here does not mean the target is gone"
        ),
        None => eprintln!(
            "phux: could not resolve the target: this server's view of the fleet is \
             incomplete, so a miss here does not mean the target is gone"
        ),
    }
    for notice in degradation.notices() {
        eprintln!("phux:   unreachable — {notice}");
    }
    degraded_status
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::{
        EXIT_PARTIAL_VIEW, miss_error, report_target_miss, report_target_miss_for,
        report_target_miss_keeping_status,
    };
    use crate::commands::json_err::codes;
    use phux_client::state::Degradation;
    use phux_protocol::wire::frame::{ErrorCode, FrameKind};
    use std::process::ExitCode;

    fn degraded() -> Degradation {
        Degradation::from_interleaved(&[FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite build-box is unreachable: link is down".to_owned(),
        }])
    }

    #[test]
    fn a_miss_against_a_whole_fleet_is_still_a_plain_failure() {
        // The unchanged path: nothing about federation should make an
        // ordinary typo cost a different exit code.
        let code = report_target_miss(Some("@9"), &Degradation::default());
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn a_miss_against_a_partial_fleet_gets_its_own_status() {
        // "no such pane" and "I could not see the half of the fleet your pane
        // is on" must not be the same answer — including to a script, which
        // reads only the number.
        let code = report_target_miss(Some("@9"), &degraded());
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::from(EXIT_PARTIAL_VIEW))
        );
        assert_ne!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    /// phux-i0e8.8.2: the JSON contract's half of the same distinction. A
    /// whole-fleet miss is `no_such_target` exit 1; a partial-view miss is
    /// `partial_view` exit 3 — and the message still never claims absence.
    #[test]
    fn json_miss_errors_split_no_such_target_from_partial_view() {
        let (err, exit_code) = miss_error(Some("@9"), &Degradation::default(), EXIT_PARTIAL_VIEW);
        assert_eq!(err.code, codes::NO_SUCH_TARGET);
        assert_eq!(exit_code, 1);
        assert_eq!(err.message, "no such target: @9");
        assert!(!err.remedy.is_empty());

        let (err, exit_code) = miss_error(Some("@9"), &degraded(), EXIT_PARTIAL_VIEW);
        assert_eq!(err.code, codes::PARTIAL_VIEW);
        assert_eq!(exit_code, EXIT_PARTIAL_VIEW);
        assert!(
            !err.message.contains("no such target"),
            "a partial-view miss must not claim absence: {}",
            err.message
        );
        assert!(err.remedy.contains("build-box"), "{}", err.remedy);
    }

    /// The json emitter path end-to-end: a partial-view miss under `--json`
    /// exits [`EXIT_PARTIAL_VIEW`], same as the prose path.
    #[test]
    fn json_partial_view_miss_exits_three() {
        let code = report_target_miss_for(true, Some("@9"), &degraded());
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::from(EXIT_PARTIAL_VIEW))
        );
    }

    /// The keeping-status variant keeps the code (`partial_view`) in the
    /// document while the process status stays 1 — the child's exit space
    /// is not spent, but the machine reader still learns the reason.
    #[test]
    fn json_keeping_status_miss_keeps_exit_one_but_names_partial_view() {
        let (err, exit_code) = miss_error(Some("@9"), &degraded(), 1);
        assert_eq!(err.code, codes::PARTIAL_VIEW);
        assert_eq!(exit_code, 1);
    }

    #[test]
    fn the_shared_resolver_keeps_its_status_and_still_refuses_to_claim_absence() {
        // `run` keeps status 1; the sentence must still differ.
        let code = report_target_miss_keeping_status(Some("@9"), &degraded());
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
        assert_ne!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::from(EXIT_PARTIAL_VIEW)),
            "a resolver shared with `run` must not spend the child's code space"
        );
    }
}
