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

/// Exit status for "I could not answer, because I could not see all of the
/// fleet".
///
/// Distinct from the established codes so a script can branch: `0` success,
/// `1` a genuine miss or no server, `2` a server-side refusal, `3` an answer
/// this client declines to give because the world it was resolved against was
/// incomplete. A retry once the satellite link is back is the right response
/// to `3` and the wrong response to `1`, which is the whole reason they are
/// not the same number.
pub(crate) const EXIT_PARTIAL_VIEW: u8 = 3;

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
    use super::{EXIT_PARTIAL_VIEW, report_target_miss, report_target_miss_keeping_status};
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
