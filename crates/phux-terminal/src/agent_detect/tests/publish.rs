use super::*;

/// Attention states publish on the first tick, under the manifest's kind
/// and name.
#[test]
fn attention_states_publish_on_the_first_tick() {
    for (screen, state) in [
        ("BLOCKED", DetectedState::Blocked),
        ("WORKING", DetectedState::Working),
        ("DONE", DetectedState::Done),
    ] {
        let mut h = Harness::new().past_grace();
        let DetectOutcome::Publish(report) = h.tick(screen) else {
            panic!("{screen} must publish");
        };
        assert_eq!((report.state, report.kind.as_str()), (state, "t"));
        assert_eq!(report.name, "t-agent");
    }
}

#[test]
fn hook_done_is_immediate_and_the_next_screen_can_supersede_it() {
    let mut h = Harness::new().past_grace();
    let report = h
        .detector
        .report_hook_state(DetectedState::Done, h.now)
        .expect("new edge");
    assert_eq!(report.state, DetectedState::Done);
    assert_eq!(h.state(), Some(DetectedState::Done));
    assert_eq!(
        published(&h.tick("WORKING")),
        DetectedState::Working,
        "hook evidence must not stand screen derivation down"
    );
}

#[test]
fn hook_state_waits_for_the_first_identity_poll() {
    let mut h = Harness::unidentified();
    assert_eq!(
        h.detector.report_hook_state(DetectedState::Done, h.now),
        None
    );
    h.launch_agent("t");
    assert_eq!(published(&h.actor_tick("WORKING")), DetectedState::Done);
}

#[test]
fn pre_identity_hook_expires_before_an_unrelated_future_occupant() {
    let mut h = Harness::unidentified();
    assert_eq!(
        h.detector.report_hook_state(DetectedState::Done, h.now),
        None
    );
    h.now += HOOK_IDENTITY_GRACE + Duration::from_millis(1);
    h.launch_agent("t");
    assert!(matches!(h.actor_tick("WORKING"), DetectOutcome::Quiet));
    assert_eq!(h.state(), None);
}

/// Ambiguous `working -> idle`: held at the fast confirming cadence, then
/// released.
#[test]
fn working_to_ambiguous_idle_is_held_for_three_confirmations() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
    assert_eq!(h.detector.interval(), TICK_IDENTIFIED);

    for i in 1..IDLE_CONFIRMATIONS {
        assert_eq!(
            h.tick("nothing matches here"),
            DetectOutcome::Quiet,
            "confirmation {i} must not publish yet",
        );
        assert_eq!(h.state(), Some(DetectedState::Working), "badge still held");
        assert_eq!(h.detector.interval(), TICK_CONFIRMING, "look again quickly");
    }
    let out = h.tick("nothing matches here");
    assert_eq!(
        published(&out),
        DetectedState::Idle,
        "released on the third"
    );
}

/// Positive idle evidence bypasses the hold.
#[test]
fn visible_idle_bypasses_the_hold() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
    let out = h.tick("IDLE");
    assert_eq!(published(&out), DetectedState::Idle, "no debounce");
    assert_eq!(h.detector.interval(), TICK_IDENTIFIED);
}

/// The hold is capped in wall-clock time.
#[test]
fn the_hold_is_capped_in_wall_clock_time() {
    let mut h = Harness::new().past_grace();
    h.tick("WORKING");
    // One ambiguous tick to open the hold ...
    assert_eq!(h.tick("nothing"), DetectOutcome::Quiet);
    // Past the cap, the next tick releases.
    h.now += super::super::IDLE_HOLD_CAP;
    let lines = vec!["nothing".to_owned()];
    let out = h.detector.tick(h.now, None, "", "", Some(&lines));
    assert_eq!(published(&out), DetectedState::Idle);
}

/// Blocked interrupts a pending idle hold immediately — attention wins.
#[test]
fn blocked_during_the_idle_hold_publishes_at_once() {
    let mut h = Harness::new().past_grace();
    h.tick("WORKING");
    assert_eq!(h.tick("nothing"), DetectOutcome::Quiet);
    let out = h.tick("BLOCKED");
    assert_eq!(published(&out), DetectedState::Blocked);
    assert!(h.detector.pending_idle.is_none(), "hold cleared");
}

/// Idle from a non-working state is not debounced.
#[test]
fn idle_from_blocked_is_not_debounced() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("BLOCKED")), DetectedState::Blocked);
    let out = h.tick("nothing matches");
    assert_eq!(published(&out), DetectedState::Idle);
}

/// A pager carries no information: freeze.
#[test]
fn skip_state_update_freezes_the_previous_state() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
    assert_eq!(h.tick("PAGER"), DetectOutcome::Quiet);
    assert_eq!(h.state(), Some(DetectedState::Working), "badge frozen");
    // And it does not even start an idle hold.
    assert!(h.detector.pending_idle.is_none());
    // Leaving the pager resumes normal derivation.
    assert_eq!(published(&h.tick("IDLE")), DetectedState::Idle);
}

/// A freeze rule outranks a matching blocked rule.
#[test]
fn freeze_outranks_every_state_bearing_rule() {
    let mut h = Harness::new().past_grace();
    assert_eq!(h.tick("PAGER and BLOCKED"), DetectOutcome::Quiet);
    assert_eq!(h.state(), None);
}

/// The fail-safe: no matching rule means idle, never blocked.
#[test]
fn an_identified_agent_with_no_matching_rule_is_idle_never_blocked() {
    let mut h = Harness::new().past_grace();
    let out = h.tick("total gibberish that matches nothing");
    assert_eq!(published(&out), DetectedState::Idle);
    assert_ne!(h.state(), Some(DetectedState::Blocked));
}

/// A long run of one state produces exactly one write.
#[test]
fn a_long_run_publishes_exactly_once() {
    for screen in ["WORKING", "IDLE"] {
        let mut h = Harness::new().past_grace();
        let publishes = (0..10)
            .filter(|_| matches!(h.tick(screen), DetectOutcome::Publish(_)))
            .count();
        assert_eq!(publishes, 1, "{screen}: only the transition is an event");
    }
}

/// A splash screen containing a blocked word does not publish in grace.
#[test]
fn the_startup_grace_suppresses_publication() {
    let mut h = Harness::new();
    // `elapsed` tracks where the next tick lands, never the boundary.
    let mut elapsed = TICK_IDENTIFIED;
    while elapsed < STARTUP_GRACE {
        assert_eq!(h.tick("BLOCKED"), DetectOutcome::Quiet, "silent in grace");
        elapsed += TICK_IDENTIFIED;
    }
    assert_eq!(h.state(), None);
    // Once the grace expires, the very same screen publishes.
    h.now += STARTUP_GRACE;
    let lines = vec!["BLOCKED".to_owned()];
    let out = h.detector.tick(h.now, None, "", "", Some(&lines));
    assert_eq!(published(&out), DetectedState::Blocked);
}

/// With the scan skipped, the detector holds and says nothing.
#[test]
fn a_skipped_scan_holds_the_last_state_and_is_quiet() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
    assert_eq!(h.tick_no_scan(), DetectOutcome::Quiet);
    assert_eq!(h.state(), Some(DetectedState::Working));
}

#[test]
fn wants_screen_skips_the_scan_whenever_the_grid_is_clean() {
    let mut h = Harness::new().past_grace();
    h.tick("IDLE");
    assert!(!h.detector.wants_screen(false), "idle + clean => skip");
    assert!(h.detector.wants_screen(true), "dirty => scan");
}

/// A static `blocked` prompt with a clean grid is not rescanned.
#[test]
fn a_blocked_pane_with_a_clean_grid_does_not_rescan() {
    let mut h = Harness::new().past_grace();
    assert_eq!(published(&h.tick("BLOCKED")), DetectedState::Blocked);
    assert!(
        !h.detector.wants_screen(false),
        "a clean grid cannot yield a different derivation, blocked or not",
    );
    // ... and the held state is still correct, and still silent.
    assert_eq!(h.tick_no_scan(), DetectOutcome::Quiet);
    assert_eq!(h.state(), Some(DetectedState::Blocked), "badge held");
    // The instant bytes arrive, it looks again.
    assert!(h.detector.wants_screen(true));
}

/// A freshly identified pane scans even with a clean grid.
#[test]
fn a_freshly_identified_pane_scans_even_with_a_clean_grid() {
    let h = Harness::new().past_grace();
    assert!(
        h.detector.wants_screen(false),
        "nothing derived yet: the detector has no state to hold",
    );
}

/// An unidentified pane does not scan, ticks slowly, and never publishes
/// or retracts.
#[test]
fn an_unidentified_pane_is_silent() {
    let mut h = Harness::unidentified();
    assert!(!h.detector.wants_screen(true), "nothing to derive against");
    assert_eq!(h.detector.interval(), super::super::TICK_UNIDENTIFIED);
    for i in 0..5 {
        let at = h.now + Duration::from_secs(i * 6);
        assert_eq!(
            h.detector.tick(at, None, "", "", None),
            DetectOutcome::Quiet
        );
    }
}
