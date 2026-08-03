//! Saying, at the user's eye level, that an answer is partial.
//!
//! # The failure this closes
//!
//! A federation hub answers `GET_STATE` with a *merged* snapshot and pushes
//! one uncorrelated `ERROR` per satellite it could not reach ahead of the ack
//! (`handle_get_state_federated`, "observable degradation, not silence"). The
//! client now keeps those notices —
//! [`phux_client::state::StateView`] carries them next to the snapshot — but
//! carrying is not showing. They were logged at `tracing::warn!`, and a CLI
//! verb installs no subscriber by default, so `phux ls` against a
//! half-reachable fleet printed a listing indistinguishable from a complete
//! one. Every verb below it inherited the same confident silence.
//!
//! # The distinction this module exists to draw
//!
//! Only *terminals* aggregate across a federation. `handle_get_state_federated`
//! discards a satellite's `sessions` and `windows` lists outright — their
//! `u32` ids would collide with the hub's — so an unreachable satellite can
//! neither add nor hide a session. That splits the verbs in two, and the split
//! is what decides how loud each one has to be:
//!
//! - **Verbs that read.** `phux ls` enumerates. A partial enumeration is
//!   still true about everything in it, so the answer stands: warn on stderr
//!   (and, under `--json`, in the payload's `unreachable` list — stderr is not
//!   a machine channel) and exit 0. Failing here would make a dead satellite
//!   in some other datacenter break the listing of the panes on this laptop.
//!
//! - **Verbs that resolve a Terminal target.** `kill`, `tag`, `agent set` and
//!   friends *search* `snapshot.panes` and act on what they find. Against a
//!   degraded snapshot, finding nothing has two completely different causes
//!   that the old code collapsed into one sentence:
//!
//!   1. the target does not exist — `no such target`, exit 1; or
//!   2. the target may exist on the half of the fleet this server could not
//!      look at.
//!
//!   Reporting (2) as (1) tells a user that a pane they can see in another
//!   window is gone, and invites the scripted follow-up ("not there? recreate
//!   it") that a transient satellite outage should never trigger. So a miss
//!   against a partial view gets its own sentence and its own status,
//!   [`EXIT_PARTIAL_VIEW`], and deliberately does *not* say "no such target".
//!
//! A resolution that *hits* under degradation still warns: a set-valued
//! selector (`#tag`, a whole session) may have matched a strict subset of what
//! it would have matched with the fleet whole, and the user is about to act on
//! that subset.

use std::process::ExitCode;

use phux_client::state::Degradation;

use crate::commands::json_err::{self, CliError, codes};

/// Exit status for "I could not answer, because I could not see all of the
/// fleet".
///
/// Distinct from the established codes so a script can branch: `0` success,
/// `1` a genuine miss or no server, `2` a server-side refusal, `3` an answer
/// this client declines to give because the world it was resolved against was
/// incomplete. A retry once the satellite link is back is the right response
/// to `3` and the wrong response to `1`, which is the whole reason they are
/// not the same number. The value lives in the canonical table
/// (`crate::exit_codes`, phux-i0e8.11.4); this is the historical name the
/// selector paths consume it under.
pub(crate) use crate::exit_codes::EXIT_PARTIAL_VIEW;

/// Warn on stderr, once per unreachable satellite, that `verb` acted on a
/// partial view of the fleet.
///
/// A no-op when the view is complete, which is every non-federated server and
/// every healthy hub — so call sites need no `if`.
pub(crate) fn warn_partial_view(verb: &str, degradation: &Degradation) {
    for notice in degradation.notices() {
        eprintln!("phux: warning: {verb} saw only part of the fleet — {notice}");
    }
}

/// Report a selector that matched nothing, saying *which* of the two reasons
/// it was, and return the matching exit status — [`EXIT_PARTIAL_VIEW`] when
/// the view was partial, `1` when it was whole.
///
/// `target` is the user's own text when the verb has one; `None` is the
/// focused-pane default, where there is no selector to quote back.
pub(crate) fn report_target_miss(target: Option<&str>, degradation: &Degradation) -> ExitCode {
    report_miss(target, degradation, ExitCode::from(EXIT_PARTIAL_VIEW))
}

/// The same message, but the established `1` in every case.
///
/// For verbs whose exit status is already spoken for by something else and
/// cannot spare a third value: `phux run` mirrors *the child's* code into
/// `0..=255`, so a `3` from here would be indistinguishable from a command
/// that legitimately exited 3 — the exact collision `run` reserves `125` to
/// avoid. `wait` has the same shape with `124`.
///
/// The distinction that matters most survives regardless: the sentence on
/// stderr still refuses to say the target is absent. Only the machine-readable
/// half is given up, and only where it was never free to take.
pub(crate) fn report_target_miss_keeping_status(
    target: Option<&str>,
    degradation: &Degradation,
) -> ExitCode {
    report_miss(target, degradation, ExitCode::FAILURE)
}

/// Json-aware sibling of [`report_target_miss`] (phux-i0e8.8.2).
///
/// Without `json`, identical to it. With `json`, the same two-way
/// distinction lands in the machine channel: a miss against a whole fleet
/// emits [`codes::NO_SUCH_TARGET`] with exit 1; a miss against a partial
/// view emits [`codes::PARTIAL_VIEW`] with exit [`EXIT_PARTIAL_VIEW`], and
/// the message still refuses to claim absence.
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

/// Json-aware sibling of [`report_target_miss_keeping_status`]: the code in
/// the document still says [`codes::PARTIAL_VIEW`] when the view was
/// partial, but the process status stays `1` for the verbs whose exit space
/// is already spoken for (`run` mirrors the child, `wait` owns 124). The
/// machine reader gets the distinction from `error.code`; the number is
/// deliberately not spent.
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

/// The [`CliError`] (and exit code) for a selector miss, pure for tests.
///
/// `degraded_status` is what a partial-view miss exits with; a whole-fleet
/// miss is always a plain `1`.
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
        // `phux run` mirrors the child's exit code, so the shared single-pane
        // resolver cannot start returning 3: a command that legitimately
        // exits 3 would become indistinguishable from a resolve failure. The
        // status stays 1; what must NOT survive that compromise is the
        // sentence, and this pins the two apart.
        let code = report_target_miss_keeping_status(Some("@9"), &degraded());
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
        assert_ne!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::from(EXIT_PARTIAL_VIEW)),
            "a resolver shared with `run` must not spend the child's code space"
        );
    }
}
