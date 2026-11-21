use super::*;

// --- pid reuse (phux-w7z2.43) -------------------------------------------

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
        reidentified("t"),
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
        reidentified("u"),
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
    }

    assert_eq!(
        h.identity_tick(),
        reidentified("t"),
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
        h.detector.identified_kind(),
        Some("t"),
        "still believed present"
    );
    assert_eq!(h.state(), Some(DetectedState::Working), "badge untouched");
}

/// A correction is alone in its tick, and the new occupant gets its own
/// startup grace.
#[test]
fn a_new_occupant_re_anchors_the_startup_grace() {
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
