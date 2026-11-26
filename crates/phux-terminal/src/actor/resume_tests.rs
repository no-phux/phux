//! Actor-level `resume=` checks. `feed_for_test` has no PTY, so each case
//! injects the foreground job instead of spawning one.

use super::{job_direct_exec, job_grandchild, job_shell_foreground, job_under_shell};

fn pane(
    job: super::ResumeJob,
) -> (
    crate::TerminalActor,
    tokio::sync::watch::Receiver<Option<phux_core::ResumeToken>>,
) {
    let mut actor = crate::TerminalActor::new(20, 5).expect("new").actor;
    actor.set_resume_job_for_test(job);
    let rx = actor.subscribe_resume();
    (actor, rx)
}

const ONE: &[u8] = b"\x1b]7501;state=working:app=a:resume=one\x07";
const TWO: &[u8] = b"\x1b]7501;state=working:app=a:resume=two\x07";

fn one() -> Option<phux_core::ResumeToken> {
    phux_core::ResumeToken::new("a", "one")
}

#[test]
fn resume_is_rejected_while_the_shell_is_foreground() {
    let (mut actor, rx) = pane(job_shell_foreground());
    actor.feed_for_test(ONE);
    assert_eq!(*rx.borrow(), None);
    assert!(!rx.has_changed().expect("open"));

    let (mut actor, mut rx) = pane(job_under_shell(true));
    actor.feed_for_test(ONE);
    assert_eq!(*rx.borrow_and_update(), one());
    actor.set_resume_job_for_test(job_shell_foreground());
    actor.feed_for_test(TWO);
    assert_eq!(*rx.borrow(), one());
    assert!(!rx.has_changed().expect("open"), "the previous token stays");
}

#[test]
fn resume_is_rejected_from_a_background_or_grandchild_writer() {
    let (mut actor, rx) = pane(job_grandchild());
    actor.feed_for_test(ONE);
    assert_eq!(*rx.borrow(), None);

    let (mut actor, mut rx) = pane(job_under_shell(true));
    actor.feed_for_test(ONE);
    assert_eq!(*rx.borrow_and_update(), one());
    let mut unseen = job_grandchild();
    unseen.fg_pgid = None;
    unseen.parent = None;
    unseen.start_ms = None;
    actor.set_resume_job_for_test(unseen);
    actor.feed_for_test(TWO);
    assert_eq!(*rx.borrow(), one());
    assert!(!rx.has_changed().expect("open"));

    actor.set_resume_job_for_test(job_grandchild());
    actor.feed_for_test(TWO);
    assert_eq!(*rx.borrow(), one(), "a grandchild does not replace it");
    assert!(!rx.has_changed().expect("open"));
}

#[test]
fn a_forged_prompt_does_not_wipe_a_stored_resume_token() {
    let (mut actor, mut rx) = pane(job_under_shell(true));
    let mut status = actor.subscribe_program_status();
    actor.feed_for_test(ONE);
    assert_eq!(*rx.borrow_and_update(), one());
    assert!(!status.borrow().is_empty());
    actor.feed_for_test(b"\x1b]133;A\x07");
    assert_eq!(*rx.borrow(), one());
    assert!(
        status.borrow_and_update().is_empty(),
        "working and blocked records still expire"
    );
}

#[test]
fn a_prompt_and_a_resume_token_in_one_burst_keep_the_token() {
    let (mut actor, mut rx) = pane(job_under_shell(true));
    let mut status = actor.subscribe_program_status();
    actor.feed_for_test(b"\x1b]7501;state=working:app=a:resume=one\x07\x1b]133;A\x07");
    assert_eq!(*rx.borrow_and_update(), one());
    assert!(status.borrow_and_update().is_empty());
}

#[test]
fn a_direct_exec_foreground_program_can_set_resume() {
    let (mut actor, mut rx) = pane(job_direct_exec());
    actor.feed_for_test(ONE);
    assert_eq!(*rx.borrow_and_update(), one());
}

#[test]
fn the_owning_job_exiting_forgets_the_token() {
    let (mut actor, mut rx) = pane(job_under_shell(true));
    actor.feed_for_test(ONE);
    assert_eq!(*rx.borrow_and_update(), one());
    // Stopped (Ctrl-Z): the owner is still alive, and a prompt must not clear it.
    actor.feed_for_test(b"\x1b]133;A\x07");
    assert_eq!(*rx.borrow(), one());
    actor.set_resume_job_for_test(job_under_shell(false));
    actor.feed_for_test(b"\r\n");
    assert_eq!(*rx.borrow_and_update(), None);
}

#[test]
fn an_empty_resume_still_withdraws_when_the_shell_is_foreground() {
    let (mut actor, mut rx) = pane(job_under_shell(true));
    actor.feed_for_test(ONE);
    assert_eq!(*rx.borrow_and_update(), one());
    actor.set_resume_job_for_test(job_shell_foreground());
    actor.feed_for_test(b"\x1b]7501;resume=\x07");
    assert_eq!(*rx.borrow_and_update(), None);
}
