use super::*;

// --- the mid-pane agent launch (the dominant interactive flow) ---------

/// An agent launched at a shell that paints a dialog and goes silent must
/// not latch to `idle`: the identifying tick reads no screen and must
/// neither publish nor consume the dirty flag.
#[test]
fn a_mid_pane_agent_launch_does_not_latch_to_idle() {
    let mut h = Harness::unidentified();

    // Two minutes of plain shell. The acquire window lapsed long ago.
    for _ in 0..240 {
        assert_eq!(h.actor_tick("$ "), DetectOutcome::Quiet);
    }

    h.launch_agent("t");
    assert!(
        !h.detector.wants_screen(h.dirty),
        "unidentified: there is nothing to derive against, so no scan",
    );

    assert_eq!(
        h.actor_tick("BLOCKED"),
        DetectOutcome::Quiet,
        "no screen was read: hold, do not invent `idle` from zero evidence",
    );
    assert!(
        h.dirty,
        "a tick that performed no scan must not consume the evidence that a scan is owed",
    );

    // Now run for three minutes of a static, silent, blocked screen.
    let mut publishes = Vec::new();
    for _ in 0..600 {
        if let DetectOutcome::Publish(report) = h.actor_tick("BLOCKED") {
            publishes.push(report.state);
        }
    }
    assert_eq!(
        publishes,
        vec![DetectedState::Blocked],
        "the truth, published exactly once — never a fabricated `idle`",
    );
    assert_eq!(h.state(), Some(DetectedState::Blocked));
}

/// A screen-less tick with nothing derived holds instead of guessing.
#[test]
fn a_screenless_tick_with_nothing_derived_publishes_nothing() {
    let mut h = Harness::new().past_grace();
    assert_eq!(h.detector.current, None, "nothing derived yet");
    assert_eq!(
        h.tick_no_scan(),
        DetectOutcome::Quiet,
        "zero evidence: hold, do not invent `idle`",
    );
    assert_eq!(h.state(), None, "nothing was published");
    assert!(
        h.detector.wants_screen(false),
        "and it still owes itself a scan — the guess must not latch",
    );
}

/// The startup grace is anchored at identification, not pane creation.
#[test]
fn the_startup_grace_is_anchored_at_identification_not_pane_creation() {
    let mut h = Harness::unidentified();
    h.now += Duration::from_secs(600);
    h.launch_agent("t");

    assert_eq!(h.actor_tick("BLOCKED"), DetectOutcome::Quiet, "identifying");
    for i in 1..10 {
        assert_eq!(
            h.actor_tick("BLOCKED"),
            DetectOutcome::Quiet,
            "tick {i} lands inside the grace: the splash must not flash `blocked`",
        );
    }
    assert_eq!(h.state(), None, "nothing was published while it painted");

    // And the grace does expire.
    assert_eq!(published(&h.actor_tick("BLOCKED")), DetectedState::Blocked);
}

// --- the edge filter is a model of OUR emissions, not of the store ------

/// After the store changes, an unchanged state is republished once.
#[test]
fn invalidating_the_edge_filter_republishes_an_unchanged_state() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("IDLE")), DetectedState::Idle);
    assert_eq!(
        h.tick("IDLE"),
        DetectOutcome::Quiet,
        "steady state is quiet"
    );

    // `phux agent clear`: the row is gone from the store.
    h.detector.invalidate_published();

    assert_eq!(
        published(&h.tick("IDLE")),
        DetectedState::Idle,
        "the detector resumes: the record comes back on the next tick",
    );
    assert_eq!(
        h.tick("IDLE"),
        DetectOutcome::Quiet,
        "and it is a re-arm, not a repeat — the filter closes again at once",
    );
}

/// The same without a scan (an idle agent's clean grid).
#[test]
fn an_invalidated_idle_agent_republishes_without_a_scan() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("IDLE")), DetectedState::Idle);
    assert!(
        !h.detector.wants_screen(false),
        "idle + clean grid: the scan is skipped, as designed",
    );
    h.detector.invalidate_published();
    assert_eq!(published(&h.tick_no_scan()), DetectedState::Idle);
}

// --- freeze ------------------------------------------------------------

/// A pager during an idle hold drops the hold and the fast cadence.
#[test]
fn a_freeze_during_the_idle_hold_drops_the_hold_and_the_fast_cadence() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
    // The turn ends ambiguously: the hold opens, the cadence goes fast.
    assert_eq!(h.tick("nothing matches"), DetectOutcome::Quiet);
    assert_eq!(h.detector.interval(), TICK_CONFIRMING);
    assert!(h.detector.pending_idle.is_some(), "holding");

    // The user hits ctrl+o and reads the transcript for three minutes.
    for _ in 0..600 {
        assert_eq!(h.tick("PAGER"), DetectOutcome::Quiet);
    }
    assert!(
        h.detector.pending_idle.is_none(),
        "the in-flight hold is abandoned, not pinned",
    );
    assert_eq!(
        h.detector.interval(),
        TICK_IDENTIFIED,
        "and the cadence falls back to the settled one",
    );
    assert!(
        !h.detector.wants_screen(false),
        "a frozen screen with a clean grid is not re-projected 10x a second",
    );
    assert_eq!(
        h.state(),
        Some(DetectedState::Working),
        "the badge is still frozen exactly where it was",
    );

    // Closing the pager restarts the hold from scratch.
    for i in 1..IDLE_CONFIRMATIONS {
        assert_eq!(
            h.tick("nothing matches"),
            DetectOutcome::Quiet,
            "confirmation {i} of the restarted hold",
        );
    }
    assert_eq!(published(&h.tick("nothing matches")), DetectedState::Idle);
}
