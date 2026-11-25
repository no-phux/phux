use super::super::*;
use std::time::{Duration, Instant};

/// Terminal 1 attached showing `committed`.
fn committed() -> (EngineHandle, Arc<Publication>, ResourceId) {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"committed");
    (owner, publication, terminal)
}

/// [`committed`], read, then a DEC 2026 hold whose watchdog already
/// expired; returns the frame that expiry published.
fn expired_hold() -> (EngineHandle, Arc<Publication>, ResourceId, Arc<GridFrame>) {
    let (owner, publication, terminal) = committed();
    publication.acquire(&terminal).unwrap();
    apply_ok(&owner, output(&terminal, 1, b"\x1b[?2026h"));
    owner.expire_presentation(owner.presentation_deadline().unwrap());
    let committed = publication.acquire(&terminal).unwrap();
    (owner, publication, terminal, committed)
}

#[test]
fn synchronized_redraw_keeps_committed_default_and_view_frames() {
    let (owner, publication, terminal) = committed();
    let source = owner.view_source(&terminal).unwrap();
    let view = owner.create_view(&source).unwrap();
    let first = publication.acquire(&terminal).unwrap();
    let view_first = publication.acquire_view(view).unwrap();
    for (seq, bytes) in [
        (1, b"\x1b[?2026h\x1b[2J\x1b[H".as_slice()),
        (2, b"replacement".as_slice()),
    ] {
        apply_ok(&owner, output(&terminal, seq, bytes));
        assert!(!owner.republish(&terminal).unwrap());
        owner.scroll(&terminal, Scroll::Bottom).unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &publication.acquire(&terminal).unwrap()
        ));
        assert!(Arc::ptr_eq(
            &view_first,
            &publication.acquire_view(view).unwrap()
        ));
    }
    apply_ok(&owner, output(&terminal, 3, b"\x1b[?2026l"));
    assert_eq!(
        publication.acquire(&terminal).unwrap().text(),
        "replacement"
    );
    assert_eq!(
        publication.acquire_view(view).unwrap().text(),
        "replacement"
    );
    assert!(owner.presentation_deadline().is_none());
}

#[test]
fn unread_catch_up_cannot_publish_an_incomplete_redraw() {
    let (owner, publication, terminal) = committed();
    let source = owner.view_source(&terminal).unwrap();
    let view = owner.create_view(&source).unwrap();
    // First defer a normal update, leaving a stale slot before DEC 2026.
    apply_ok(&owner, output(&terminal, 1, b"!"));
    apply_ok(&owner, output(&terminal, 2, b"\x1b[?2026h\x1b[2J\x1b[H"));
    let held = publication.acquire(&terminal).unwrap();
    assert_eq!(held.text(), "committed");
    assert_eq!(publication.acquire_view(view).unwrap().text(), "committed");
    apply_ok(&owner, output(&terminal, 3, b"complete\x1b[?2026l"));
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "complete");
    assert_eq!(publication.acquire_view(view).unwrap().text(), "complete");
}

#[test]
fn a_batched_end_and_new_begin_rearms_an_expired_watchdog() {
    let (owner, publication, terminal, committed) = expired_hold();
    apply_batch_ok(
        &owner,
        vec![
            output(&terminal, 2, b"\x1b[?2026l"),
            output(&terminal, 3, b"\x1b[?2026h\x1b[2J"),
        ],
    );
    assert!(owner.presentation_deadline().is_some());
    assert!(Arc::ptr_eq(
        &committed,
        &publication.acquire(&terminal).unwrap()
    ));
    apply_ok(&owner, output(&terminal, 4, b"\x1b[Hcomplete\x1b[?2026l"));
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "complete");
}

#[test]
fn end_and_new_begin_inside_one_payload_rearm_an_expired_watchdog() {
    let (owner, publication, terminal, committed) = expired_hold();
    apply_ok(
        &owner,
        output(&terminal, 2, b"\x1b[?2026l\x1b[?2026h\x1b[2J"),
    );
    assert!(owner.presentation_deadline().is_some());
    assert!(Arc::ptr_eq(
        &committed,
        &publication.acquire(&terminal).unwrap()
    ));
}

#[test]
fn uncommitted_batched_transactions_cannot_postpone_the_watchdog() {
    let (owner, publication, terminal) = committed();
    publication.acquire(&terminal).unwrap();
    apply_ok(&owner, output(&terminal, 1, b"\x1b[?2026h\x1b[2J"));
    let deadline = owner.presentation_deadline().unwrap();
    for n in 0..5 {
        apply_batch_ok(
            &owner,
            vec![
                output(&terminal, 2 + n * 2, b"\x1b[?2026l"),
                output(&terminal, 3 + n * 2, b"\x1b[?2026h"),
            ],
        );
        assert_eq!(owner.presentation_deadline(), Some(deadline));
        assert_eq!(publication.acquire(&terminal).unwrap().text(), "committed");
    }
    assert_eq!(owner.expire_presentation(deadline), vec![terminal]);
    assert!(owner.presentation_deadline().is_none());
}

#[test]
fn missing_end_is_released_by_the_deadline_lane_without_more_output() {
    let (owner, publication, terminal) = committed();
    publication.acquire(&terminal).unwrap();
    apply_ok(
        &owner,
        output(&terminal, 1, b"\x1b[?2026h\x1b[2J\x1b[Hwaiting"),
    );
    let deadline = owner.presentation_deadline().unwrap();
    assert!(
        owner
            .expire_presentation(deadline.checked_sub(Duration::from_millis(1)).unwrap())
            .is_empty()
    );
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "committed");
    assert_eq!(owner.expire_presentation(deadline), vec![terminal.clone()]);
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "waiting");
    assert!(owner.presentation_deadline().is_none());
    apply_ok(&owner, output(&terminal, 2, b"!"));
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "waiting!");
    apply_ok(&owner, output(&terminal, 3, b"\x1b[?2026l"));
    apply_ok(&owner, output(&terminal, 4, b"\x1b[?2026h\x1b[2J"));
    assert!(owner.presentation_deadline().is_some());
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "waiting!");
}

#[test]
fn synchronized_bootstrap_and_new_view_wait_for_a_complete_frame() {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"\x1b[?2026hpartial");
    assert!(matches!(
        owner.view_source(&terminal),
        Err(EngineError::ProjectionUnavailable)
    ));
    assert!(publication.acquire(&terminal).is_none());
    apply_ok(
        &owner,
        output(&terminal, 1, b"\x1b[2J\x1b[Hcomplete\x1b[?2026l"),
    );
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "complete");
    let source = owner.view_source(&terminal).unwrap();
    let view = owner.create_view(&source).unwrap();
    // Registered at admission, then published once from the complete frame.
    assert_eq!(publication.view_generation(view), Some(1));
    assert_eq!(publication.acquire_view(view).unwrap().text(), "complete");
}

#[test]
fn synchronization_does_not_hold_other_terminals_or_survive_reset() {
    let (owner, publication) = owner();
    let a = id(1);
    let b = id(2);
    attach_many(&owner, &[(&a, b"first"), (&b, b"second")]);
    publication.acquire(&a).unwrap();
    publication.acquire(&b).unwrap();
    apply_ok(&owner, output(&a, 1, b"\x1b[?2026h\x1b[2J"));
    apply_ok(&owner, output(&b, 1, b"!"));
    assert_eq!(publication.acquire(&b).unwrap().text(), "second!");
    owner.reset_connection();
    assert!(owner.presentation_deadline().is_none());
    attach(&owner, &a, b"fresh");
    assert_eq!(publication.acquire(&a).unwrap().text(), "fresh");
    assert!(
        owner
            .expire_presentation(Instant::now() + Duration::from_secs(2))
            .is_empty()
    );
}

#[test]
fn replacement_generation_gets_a_fresh_watchdog_and_close_releases_it() {
    let (owner, publication, terminal) = committed();
    owner.set_cell_pixels(&terminal, 9, 18).unwrap();
    let source = owner.view_source(&terminal).unwrap();
    let view = owner.create_view(&source).unwrap();
    publication.acquire_view(view).unwrap();
    publication.acquire(&terminal).unwrap();
    apply_ok(&owner, output(&terminal, 1, b"\x1b[?2026h"));
    let old_deadline = owner.presentation_deadline().unwrap();
    owner.expire_presentation(old_deadline);
    rebootstrap(&owner, &terminal, 2, b"\x1b[?2026hnew");
    assert!(owner.presentation_deadline().is_some());
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "committed");
    assert_eq!(publication.acquire_view(view).unwrap().text(), "committed");
    assert!(
        owner.presentation_deadline().is_some(),
        "reapplying cached cell pitch must preserve the hold"
    );
    owner.set_retain_on_close(&terminal, true);
    apply_ok(&owner, EngineEvent::closed_unknown(terminal.clone()));
    assert!(owner.presentation_deadline().is_none());
    assert_eq!(publication.acquire(&terminal).unwrap().text(), "new");
    assert_eq!(publication.acquire_view(view).unwrap().text(), "new");
}

#[test]
fn a_nonretained_close_drops_held_frames_and_deadlines() {
    let (owner, publication, terminal) = committed();
    let source = owner.view_source(&terminal).unwrap();
    let view = owner.create_view(&source).unwrap();
    apply_ok(&owner, output(&terminal, 1, b"\x1b[?2026h\x1b[2J"));
    apply_ok(&owner, EngineEvent::closed_unknown(terminal.clone()));
    assert!(owner.presentation_deadline().is_none());
    assert!(publication.acquire(&terminal).is_none());
    assert!(publication.acquire_view(view).is_none());
}
