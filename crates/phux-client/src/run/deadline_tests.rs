use std::path::Path;
use std::time::Duration;

use phux_protocol::ResourceId;
use tokio::time::Instant;

use phux_core::screen::ScreenState;

use super::{RunOutcome, SUBMIT_GRACE, Submission, run_in_with_deadline};
use crate::deadline::Deadline;
use crate::testkit::{self, ScriptSpec};

const BUDGET: Duration = Duration::from_millis(300);
/// Slack for a loaded machine; a regression overruns by far more.
const TOLERANCE: Duration = Duration::from_secs(3);
/// A regression fails here instead of hanging the test run.
const WEDGE: Duration = Duration::from_secs(10);

/// `force`: a stalled peer cannot answer the precondition's reads, and
/// these cases exercise the submit/poll budget.
async fn run_with_budget(socket: &Path) -> (RunOutcome, Duration) {
    let start = Instant::now();
    let deadline = Deadline::new(Some(BUDGET));
    let run = run_in_with_deadline(socket, ResourceId::local(1), "true", "n1", deadline, true);
    let outcome = tokio::time::timeout(WEDGE, run)
        .await
        .expect("the run must return; a timeout here is the wedge itself")
        .expect("a stalled peer is a timeout, not an error");
    (outcome, start.elapsed())
}

/// Assert a timeout no sooner than `min` that reports `expected`, and
/// hand back the screen it carried.
fn assert_timed_out(
    outcome: RunOutcome,
    elapsed: Duration,
    min: Duration,
    expected: Submission,
) -> ScreenState {
    let RunOutcome::TimedOut {
        submission, screen, ..
    } = outcome
    else {
        panic!("expected a timeout, got {outcome:?}");
    };
    assert_eq!(submission, expected);
    assert!(elapsed >= min, "gave up early after {elapsed:?}");
    assert!(elapsed < min + TOLERANCE, "overran: {elapsed:?}");
    *screen
}

#[tokio::test]
async fn a_peer_that_never_answers_hello_ends_the_run_on_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (socket, peer) = testkit::spawn_hold_silent(dir.path());

    let (outcome, elapsed) = run_with_budget(&socket).await;
    assert_timed_out(outcome, elapsed, BUDGET, Submission::NotSent);
    peer.abort();
}

#[tokio::test]
async fn a_peer_that_never_answers_get_screen_ends_the_run_on_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (socket, peer) =
        testkit::spawn_serve_every(dir.path(), || ScriptSpec::new().wedge_screen_reads());

    let (outcome, elapsed) = run_with_budget(&socket).await;
    let screen = assert_timed_out(outcome, elapsed, BUDGET, Submission::Complete);
    assert!(
        screen.lines.is_empty(),
        "no read completed, so the reported screen is the empty default"
    );
    peer.abort();
}

#[tokio::test]
async fn a_peer_that_acks_the_line_but_never_the_enter_reports_partial_input() {
    // The line is acked but its Enter never is: partial, after the grace.
    let dir = tempfile::tempdir().expect("tempdir");
    let (socket, peer) =
        testkit::spawn_serve_every(dir.path(), || ScriptSpec::new().wedge_input_after(1));

    let (outcome, elapsed) = run_with_budget(&socket).await;
    assert_timed_out(outcome, elapsed, BUDGET + SUBMIT_GRACE, Submission::Partial);
    peer.abort();
}
