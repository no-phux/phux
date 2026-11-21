use super::*;

// --- occupancy: departure needs evidence, and evidence needs confirming -

/// An observed non-agent occupant retracts the record.
#[test]
fn losing_the_occupant_retracts_a_published_record() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);

    // The agent exits back to its shell.
    h.occupy(Occupancy::Vacant { pgid: 100 });
    for i in 1..VACANT_CONFIRMATIONS {
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Quiet,
            "vacancy {i} is not yet confirmed",
        );
        assert_eq!(h.state(), Some(DetectedState::Working), "badge still held");
    }
    assert_eq!(h.identity_tick(), DetectOutcome::Retract);
    assert_eq!(h.state(), None, "the badge is gone");

    // And it retracts exactly once.
    assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
}

/// Unanswerable queries are not evidence: twenty failures accumulate
/// into nothing.
#[test]
fn an_unresolved_occupancy_never_retracts() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
    h.occupy(Occupancy::Unresolved);

    for i in 0..20 {
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Quiet,
            "unanswered query {i} must change nothing",
        );
    }
    assert_eq!(
        h.detector.identified_kind(),
        Some("t"),
        "the pane is still believed to host the agent it hosted",
    );
    assert_eq!(h.state(), Some(DetectedState::Working), "badge held");

    // The failures contributed nothing to the vacancy streak.
    h.occupy(Occupancy::Vacant { pgid: 100 });
    assert_eq!(
        h.identity_tick(),
        DetectOutcome::Quiet,
        "streak starts at 0"
    );
    assert_eq!(h.identity_tick(), DetectOutcome::Retract);
}

/// One vacant observation (a brief subprocess) does not retract.
#[test]
fn a_single_vacant_observation_does_not_retract() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);

    h.occupy(Occupancy::Vacant { pgid: 100 });
    assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
    // The subprocess exits and the agent is back in the foreground.
    h.occupy(agent("t", 100));
    assert_eq!(h.identity_tick(), DetectOutcome::Quiet, "nothing happened");
    assert_eq!(h.state(), Some(DetectedState::Working), "badge untouched");

    // The streak reset too.
    h.occupy(Occupancy::Vacant { pgid: 100 });
    assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
}

// --- the occupant changed (phux-w7z2.27) --------------------------------

/// A different kind in the same pane is corrected, not left beside a
/// stale `kind`.
#[test]
fn a_different_kind_in_the_same_pane_is_a_correction() {
    let mut h = occupied("t", 100);

    h.occupy(agent("u", 200));
    assert_eq!(
        h.identity_tick(),
        reidentified("u"),
        "the pane's occupant changed and someone has to be told",
    );
}

/// A same-kind restart (new pgid) is a new occupant.
#[test]
fn a_same_kind_restart_is_a_new_occupant() {
    let mut h = occupied("t", 100);

    h.occupy(agent("t", 271));
    assert_eq!(
        h.identity_tick(),
        reidentified("t"),
        "a restart is an identity change, whatever the binary is called",
    );
}

/// The same occupant seen again is not an event.
#[test]
fn the_same_occupant_seen_again_is_not_an_event() {
    let mut h = occupied("t", 100);
    for _ in 0..20 {
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Quiet,
            "the same agent, still there, is not news",
        );
    }
    assert_eq!(h.state(), Some(DetectedState::Working));
}
