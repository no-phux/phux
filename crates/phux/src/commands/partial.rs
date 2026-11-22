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

use crate::commands::json_err::Refusal;

/// Exit status 3: no answer, because the fleet view was incomplete. Retrying
/// once the satellite is back is right for 3 and wrong for 1.
pub(crate) use crate::exit_codes::EXIT_PARTIAL_VIEW;

/// Warn once per unreachable satellite that `verb` acted on a partial view;
/// a no-op for a complete view.
pub(crate) fn warn_partial_view(verb: &str, degradation: &Degradation) {
    for notice in degradation.notices() {
        warn!("{} saw only part of the fleet — {notice}", verb);
    }
}

/// Report a selector that matched nothing, distinguishing the two causes,
/// and return [`EXIT_PARTIAL_VIEW`] or `1`. `target` is `None` for the
/// focused-pane default.
pub(crate) fn report_target_miss(target: Option<&str>, degradation: &Degradation) -> ExitCode {
    target_miss(target, degradation, EXIT_PARTIAL_VIEW).emit(false)
}

/// Json-aware [`report_target_miss`]: `no_such_target`/1 for a whole fleet,
/// `partial_view`/[`EXIT_PARTIAL_VIEW`] for a partial one.
pub(crate) fn report_target_miss_for(
    json: bool,
    target: Option<&str>,
    degradation: &Degradation,
) -> ExitCode {
    target_miss(target, degradation, EXIT_PARTIAL_VIEW).emit(json)
}

/// [`report_target_miss_for`] but always exiting `1`, for verbs whose exit
/// space is taken (`run` mirrors the child's code, `wait` owns 124): the
/// document still says `partial_view`, and the sentence still refuses to
/// call the target absent.
pub(crate) fn report_target_miss_keeping_status_for(
    json: bool,
    target: Option<&str>,
    degradation: &Degradation,
) -> ExitCode {
    target_miss_keeping_status(target, degradation).emit(json)
}

/// [`target_miss`] for the verbs whose exit space is taken: the partial
/// view still says `partial_view`, but the status stays `1`.
pub(crate) fn target_miss_keeping_status(
    target: Option<&str>,
    degradation: &Degradation,
) -> Refusal {
    target_miss(target, degradation, crate::exit_codes::EXIT_FAILURE)
}

/// The refusal for a selector miss; `degraded_status` is the partial-view
/// status. A whole-fleet miss is `no_such_target`, exit 1, worded as scripts
/// already grep for. A partial-view miss never says "no such target": this
/// client does not know that (see the module docs).
pub(crate) fn target_miss(
    target: Option<&str>,
    degradation: &Degradation,
    degraded_status: u8,
) -> Refusal {
    let err = phux_client::contract::target_miss(target, degradation);
    if degradation.is_complete() {
        return Refusal::from_error(err, crate::exit_codes::EXIT_FAILURE);
    }
    // A human reads one hint per unreachable satellite instead.
    let human = std::iter::once(err.message.clone())
        .chain(
            degradation
                .notices()
                .iter()
                .map(|notice| format!("unreachable — {notice}")),
        )
        .collect();
    Refusal::from_error(err, degraded_status).with_human(human)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::{EXIT_PARTIAL_VIEW, target_miss, target_miss_keeping_status};
    use crate::commands::json_err::codes;
    use phux_client::state::Degradation;
    use phux_protocol::wire::frame::{ErrorCode, FrameKind};

    fn degraded() -> Degradation {
        Degradation::from_interleaved(&[FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite build-box is unreachable: link is down".to_owned(),
        }])
    }

    /// phux-i0e8.8.2: the JSON contract's half of the same distinction. A
    /// whole-fleet miss is `no_such_target` exit 1; a partial-view miss is
    /// `partial_view` exit 3 — and the message still never claims absence.
    #[test]
    fn json_miss_errors_split_no_such_target_from_partial_view() {
        let refusal = target_miss(Some("@9"), &Degradation::default(), EXIT_PARTIAL_VIEW);
        let (err, exit_code) = (refusal.err, refusal.exit);
        assert_eq!(err.code, codes::NO_SUCH_TARGET);
        assert_eq!(exit_code, 1);
        assert_eq!(err.message, "no such target: @9");
        assert!(!err.remedy.is_empty());

        let refusal = target_miss(Some("@9"), &degraded(), EXIT_PARTIAL_VIEW);
        let (err, exit_code) = (refusal.err, refusal.exit);
        assert_eq!(err.code, codes::PARTIAL_VIEW);
        assert_eq!(exit_code, EXIT_PARTIAL_VIEW);
        assert!(
            !err.message.contains("no such target"),
            "a partial-view miss must not claim absence: {}",
            err.message
        );
        assert!(err.remedy.contains("build-box"), "{}", err.remedy);
    }

    /// The keeping-status variant keeps the code (`partial_view`) in the
    /// document while the process status stays 1 — the child's exit space
    /// is not spent, but the machine reader still learns the reason.
    #[test]
    fn json_keeping_status_miss_keeps_exit_one_but_names_partial_view() {
        let refusal = target_miss_keeping_status(Some("@9"), &degraded());
        let (err, exit_code) = (refusal.err, refusal.exit);
        assert_eq!(err.code, codes::PARTIAL_VIEW);
        assert_eq!(exit_code, 1);
    }
}
