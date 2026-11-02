use super::*;

// --- agent-detector dirty-flag accounting (ADR-0046) -------------------

/// `detect_tick` consumes the detector dirty flag only when it scanned; an
/// unidentified pane never scans, so the flag must survive.
#[test]
fn detect_tick_keeps_the_dirty_flag_when_it_performs_no_scan() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.agent_detect = Some(AgentDetector::new(
        crate::agent_detect::rules::global(),
        std::time::Instant::now(),
    ));

    // The agent paints. No agent is identified yet (no PTY, so identity
    // never resolves), so this tick cannot scan.
    actor.vt_write_for_test(b"a permission dialog");
    assert!(actor.agent_dirty_since_detect, "the grid changed");

    assert!(actor.detect_tick().is_some(), "the detector ran");
    assert!(
        actor.agent_dirty_since_detect,
        "a tick that performed no scan must not consume the evidence that a scan is owed",
    );
}

/// Once identified, the scan runs and consumes the flag.
#[test]
fn detect_tick_consumes_the_dirty_flag_when_it_scans() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let now = std::time::Instant::now();
    let mut detector = AgentDetector::new(crate::agent_detect::rules::global(), now);
    detector.force_identity("claude", now);
    actor.agent_detect = Some(detector);

    actor.vt_write_for_test(b"some output");
    assert!(actor.agent_dirty_since_detect);

    assert!(actor.detect_tick().is_some(), "the detector ran");
    assert!(
        !actor.agent_dirty_since_detect,
        "a scan consumes the flag; otherwise every tick re-projects the grid forever",
    );
}

#[test]
fn input_snapshot_publishes_after_seed_output_and_resize() {
    let seeded = TerminalActor::new_with_seed(80, 24, b"\x1b[?2004h").expect("seeded");
    assert!(
        seeded
            .handle
            .terminal()
            .expect("terminal facet")
            .input_snapshot
            .borrow()
            .bracketed_paste
    );

    let mut actor = seeded.actor;
    let mut snapshots = seeded
        .handle
        .terminal()
        .expect("terminal facet")
        .input_snapshot
        .clone();
    snapshots.borrow_and_update();
    actor.vt_write_for_test(b"\x1b[?1004h");
    assert!(snapshots.has_changed().expect("publisher alive"));
    assert!(snapshots.borrow_and_update().focus_reporting);

    actor.handle_resize(101, 39, Some((11, 19)));
    assert!(snapshots.has_changed().expect("publisher alive"));
    let resized = *snapshots.borrow_and_update();
    assert_eq!((resized.cols, resized.rows), (101, 39));
    assert_eq!(resized.cell_px, (11, 19));
}

/// Without a live `AgentSession`, a state record falls back to the
/// detector path (ADR-0085): a real report, not a swallowed one.
#[test]
fn a_synthesized_state_record_falls_back_to_the_detector_path() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let now = std::time::Instant::now();
    let mut detector = AgentDetector::new(crate::agent_detect::rules::global(), now);
    detector.force_identity("claude", now);
    actor.agent_detect = Some(detector);
    actor.agent_dirty_since_detect = false;

    actor
        .synthesize_state_record(crate::agent_detect::DetectedState::Blocked)
        .expect("the fallback accepts the report");

    assert_eq!(
        actor
            .agent_detect
            .as_ref()
            .and_then(AgentDetector::published_state),
        Some(crate::agent_detect::DetectedState::Blocked),
        "the hook's evidence still reaches the record",
    );
    assert!(
        actor.agent_dirty_since_detect,
        "and still forces one ordinary derivation, so it cannot latch",
    );
}

#[test]
fn program_status_bel_terminators_are_not_bells_but_ground_bel_is() {
    let mut actor = TerminalActor::new(20, 5).expect("new").actor;
    let (sink, mut events) = mpsc::channel(8);
    actor.set_event_sink(sink);
    actor.feed_for_test(b"\x1b]7501;state=working");
    actor.feed_for_test(b"\x07\x1b]7501;?\x07\x1b]99;ignored\x07");
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event.event, AgentEvent::Bell),
            "OSC terminators must not ring"
        );
    }
    actor.feed_for_test(b"\x07\x07");
    assert!(matches!(
        events.try_recv().map(|event| event.event),
        Ok(AgentEvent::Bell)
    ));
    assert!(matches!(
        events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}
