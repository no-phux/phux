use super::*;

// --- pid reuse (phux-w7z2.43) -------------------------------------------

/// A recycled pgid with a new start time is a new occupant.
#[test]
fn a_recycled_pgid_is_a_new_occupant_not_the_old_one() {
    let mut h = occupied("t", 100);

    // Same kind, same id, different process.
    h.occupy(agent_started("t", 100, Some(999_999)));
    assert_eq!(
        h.identity_tick(),
        DetectOutcome::Reidentified {
            kind: "t".to_owned(),
            name: "t-agent".to_owned(),
        },
        "the id was recycled; the start time is what says so",
    );
}

/// A stable start time never manufactures a restart.
#[test]
fn a_stable_start_time_never_manufactures_a_restart() {
    let mut h = occupied("t", 100);
    for _ in 0..20 {
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Quiet,
            "same pgid, same start time: nothing happened",
        );
    }
    assert_eq!(h.state(), Some(DetectedState::Working), "badge untouched");
}

/// Without start times, comparison degrades to pgids, not "all new".
#[test]
fn an_unavailable_start_time_degrades_to_comparing_pgids() {
    let mut h = Harness::unidentified();
    h.occupy(agent_started("t", 100, None));
    assert_eq!(h.identity_tick(), DetectOutcome::Quiet, "acquiring");
    h.now += STARTUP_GRACE + Duration::from_millis(1);
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);

    for _ in 0..20 {
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Quiet,
            "no start time anywhere: fall back to the pgid, stay silent",
        );
    }
    // ... and the pgid alone still catches the restarts it always caught.
    h.occupy(agent_started("t", 271, None));
    assert!(matches!(
        h.identity_tick(),
        DetectOutcome::Reidentified { .. }
    ));
}

/// A transiently unreadable start time does not erase the known one.
#[test]
fn a_transiently_unreadable_start_time_does_not_erase_the_one_we_have() {
    let mut h = occupied("t", 100);

    // Same pgid, failed start-time reads: same occupant, nothing emitted.
    for _ in 0..3 {
        h.occupy(agent_started("t", 100, None));
        assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
    }

    // The original start time still catches a later recycle.
    h.occupy(agent_started("t", 100, Some(999_999)));
    assert_eq!(
        h.identity_tick(),
        DetectOutcome::Reidentified {
            kind: "t".to_owned(),
            name: "t-agent".to_owned(),
        },
        "the remembered start time survived the failed queries",
    );
}

// --- the cheap pgid probe (phux-w7z2.50) --------------------------------

/// A pgid change is caught by the cheap probe on an ordinary tick.
#[test]
fn a_pgid_change_is_caught_by_the_ordinary_cadence_not_just_the_recheck() {
    let mut h = occupied("t", 100);

    // The full recheck is not due yet, so only the cheap probe can see it.
    h.occupy(agent("u", 200));
    assert_eq!(
        h.tick("WORKING"),
        DetectOutcome::Reidentified {
            kind: "u".to_owned(),
            name: "u-agent".to_owned(),
        },
        "the cheap pgid-only probe must not wait out the full recheck \
             for a change it can see for the price of one ioctl",
    );
}

/// A recycled pgid is invisible to the cheap probe but still caught on
/// the full recheck.
#[test]
fn a_recycled_pgid_is_invisible_to_the_cheap_probe_but_still_caught_on_recheck() {
    let mut h = occupied("t", 100);

    h.occupy(agent_started("t", 100, Some(999_999)));

    // Same pgid every cheap probe: stay quiet.
    for i in 0..5 {
        assert_eq!(
            h.tick("WORKING"),
            DetectOutcome::Quiet,
            "tick {i}: the cheap probe cannot afford to notice a same-pgid \
                 recycle, and must not guess",
        );
        assert_eq!(
            h.detector.identified_occupant,
            Some(Occupant::new(100, Some(700))),
            "tick {i}: still believed to be the ORIGINAL process",
        );
    }

    assert_eq!(
        h.identity_tick(),
        DetectOutcome::Reidentified {
            kind: "t".to_owned(),
            name: "t-agent".to_owned(),
        },
        "the full recheck still closes the .43 hole the cheap probe cannot",
    );
}

/// An unresolvable cheap probe holds rather than guessing.
#[test]
fn an_unresolvable_cheap_probe_holds_rather_than_guessing() {
    let mut h = occupied("t", 100);
    h.occupy(Occupancy::Unresolved);
    for _ in 0..5 {
        assert_eq!(h.tick("WORKING"), DetectOutcome::Quiet);
    }
    assert_eq!(
        h.detector.identified.as_deref(),
        Some("t"),
        "still believed present"
    );
    assert_eq!(h.state(), Some(DetectedState::Working), "badge untouched");
}

/// A new occupant gets its own startup grace.
#[test]
fn a_new_occupant_re_anchors_the_startup_grace() {
    let mut h = occupied("t", 100);
    h.occupy(agent("u", 200));
    assert!(matches!(
        h.identity_tick(),
        DetectOutcome::Reidentified { .. }
    ));

    // Its splash screen paints something a rule matches. Silence.
    let mut elapsed = TICK_IDENTIFIED;
    while elapsed < STARTUP_GRACE {
        assert_eq!(
            h.tick("WORKING"),
            DetectOutcome::Quiet,
            "the new occupant is still painting",
        );
        elapsed += TICK_IDENTIFIED;
    }
    assert_eq!(h.state(), None, "nothing published between the two agents");

    // And then the truth, attributed to the RIGHT process.
    h.now += STARTUP_GRACE;
    let lines = vec!["WORKING".to_owned()];
    match h.detector.tick(h.now, None, "", "", Some(&lines)) {
        DetectOutcome::Publish(report) => {
            assert_eq!(report.kind, "u");
            assert_eq!(report.name, "u-agent");
            assert_eq!(report.state, DetectedState::Working);
        }
        other => panic!("expected a publish for the new occupant, got {other:?}"),
    }
}

/// A correction is alone in its tick, never with a state.
#[test]
fn a_correction_short_circuits_its_own_tick() {
    let mut h = occupied("t", 100);
    h.occupy(agent("u", 200));
    assert!(matches!(
        h.identity_tick(),
        DetectOutcome::Reidentified { .. }
    ));
    assert_eq!(
        h.detector.published, None,
        "nothing about the new occupant has been published yet",
    );
}
