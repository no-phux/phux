use super::*;

/// Title rules outrank screen rules, end to end.
#[test]
fn the_title_outranks_the_screen() {
    let spec: ManifestSpec = toml::from_str(
        r#"
kind = "t"
binaries = ["t"]
[[rules]]
id = "title-working"
state = "working"
priority = 1
region = "title"
match = { contains = "busy" }
[[rules]]
id = "screen-idle"
state = "idle"
priority = 99
region = "viewport"
visible-idle = true
match = { contains = "IDLE" }
"#,
    )
    .expect("parses");
    let mut set = RuleSet::default();
    set.install(spec).expect("compiles");
    let now = Instant::now();
    let mut detector = AgentDetector::new(Rc::new(set), now);
    detector.force_identity("t", now);
    let at = now + STARTUP_GRACE + Duration::from_millis(1);
    let lines = vec!["IDLE".to_owned()];
    let out = detector.tick(at, None, "busy", "", Some(&lines));
    assert_eq!(published(&out), DetectedState::Working);
}

/// With a live session the screen cannot publish a lifecycle state, not
/// even a matched `blocked`.
#[test]
fn a_live_agent_session_suppresses_a_contradicting_screen_derivation() {
    let mut h = Harness::new().past_grace();
    h.session_open();
    assert_eq!(
        h.tick("BLOCKED"),
        DetectOutcome::Quiet,
        "the screen must not publish over a live session's stream",
    );
    assert_eq!(h.state(), None, "and must not advance its own edge filter");
    assert_eq!(h.tick("WORKING"), DetectOutcome::Quiet, "nor a second one");
    assert_eq!(h.state(), None);
}

/// A session that has said nothing leaves idle (and identity) to the
/// screen.
#[test]
fn a_live_agent_session_leaves_the_idle_confirmation_to_the_screen() {
    let mut h = Harness::new().past_grace();
    h.session_open();
    assert_eq!(
        published(&h.tick("a quiet prompt")),
        DetectedState::Idle,
        "idle is detector-owned while a session is live but silent",
    );
}

/// The fail-safe idle does not overwrite a stream's `done`.
#[test]
fn the_fail_safe_idle_does_not_overwrite_a_streams_done() {
    let mut h = Harness::new().past_grace();
    h.session_open();
    h.stream_says(DetectedState::Done);
    for _ in 0..IDLE_CONFIRMATIONS.saturating_add(2) {
        assert_eq!(h.tick("a screen no rule matches"), DetectOutcome::Quiet);
    }
    assert_eq!(h.state(), Some(DetectedState::Done));
}

/// The fail-safe idle does not overwrite a live stream's `working`.
#[test]
fn the_fail_safe_idle_does_not_overwrite_a_live_stream() {
    let mut h = Harness::new().past_grace();
    h.session_open();
    h.stream_says(DetectedState::Working);
    assert_eq!(h.state(), Some(DetectedState::Working));

    for _ in 0..IDLE_CONFIRMATIONS.saturating_add(2) {
        assert_eq!(
            h.tick("a screen no rule matches"),
            DetectOutcome::Quiet,
            "no-rule-matched is not evidence that the turn ended",
        );
    }
    assert_eq!(
        h.state(),
        Some(DetectedState::Working),
        "the stream's state survives the screen tick",
    );
}

/// Even a positive idle does not talk over a stream still `working`.
#[test]
fn a_positive_idle_does_not_overwrite_a_stream_still_working() {
    let mut h = Harness::new().past_grace();
    h.session_open();
    h.stream_says(DetectedState::Working);
    assert_eq!(h.tick("IDLE"), DetectOutcome::Quiet);
    assert_eq!(h.state(), Some(DetectedState::Working));

    h.stream_says(DetectedState::Blocked);
    assert_eq!(h.tick("IDLE"), DetectOutcome::Quiet, "nor a blocked one");
    assert_eq!(h.state(), Some(DetectedState::Blocked));
}

/// After the stream says `stop`, a positive idle publishes.
#[test]
fn a_positive_idle_publishes_once_the_stream_has_stopped() {
    let mut h = Harness::new().past_grace();
    h.session_open();
    h.stream_says(DetectedState::Working);
    h.stream_says(DetectedState::Done);
    assert_eq!(
        published(&h.tick("IDLE")),
        DetectedState::Idle,
        "after `stop` the screen owns the idle confirmation again",
    );
}

/// A retracted stream hands idle straight back to the screen.
#[test]
fn a_retracted_stream_hands_the_idle_confirmation_straight_back() {
    let mut h = Harness::new().past_grace();
    h.session_open();
    h.stream_says(DetectedState::Working);
    let now = h.now;
    h.detector.report_stream_state(None, now);
    assert_eq!(published(&h.tick("IDLE")), DetectedState::Idle);
}

/// Confirmed departure still retracts with a live session (`kill -9`
/// runs no `session_end`).
#[test]
fn a_live_agent_session_still_retracts_a_departed_agent() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
    h.session_open();

    h.occupy(Occupancy::Vacant { pgid: 100 });
    for _ in 1..VACANT_CONFIRMATIONS {
        assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
    }
    assert_eq!(
        h.identity_tick(),
        DetectOutcome::Retract,
        "a live session does not make a dead agent's badge unretractable",
    );
    assert_eq!(h.state(), None);
}

/// After `session_end` the screen resumes and reasserts once.
#[test]
fn the_screen_scrape_resumes_and_reasserts_once_when_the_session_ends() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("BLOCKED")), DetectedState::Blocked);

    h.session_open();
    assert_eq!(h.tick("BLOCKED"), DetectOutcome::Quiet, "suppressed");

    h.session_end();
    assert_eq!(
        published(&h.tick("BLOCKED")),
        DetectedState::Blocked,
        "the resumed scrape reasserts what it can now see",
    );
    assert_eq!(
        h.tick("BLOCKED"),
        DetectOutcome::Quiet,
        "exactly once: the edge filter is armed again, not disabled",
    );
}

/// Hook evidence is not suppressed by a live session.
#[test]
fn hook_evidence_is_not_suppressed_by_a_live_session() {
    let mut h = Harness::new().past_grace();
    h.session_open();
    let report = h
        .detector
        .report_hook_state(DetectedState::Blocked, h.now)
        .expect("a hook edge publishes");
    assert_eq!(report.state, DetectedState::Blocked);
}
