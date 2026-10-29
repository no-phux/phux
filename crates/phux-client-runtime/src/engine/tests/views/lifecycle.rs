use super::*;
#[test]
fn gestures_prediction_and_teardown_are_isolated() {
    let (owner, publication) = owner();
    let terminal = id(104);
    attach(&owner, &terminal, b"abc");
    let a = owner.create_view(&terminal).unwrap();
    let b = owner.create_view(&terminal).unwrap();
    let press = owner.view_selection_gesture(a, gesture(0, 0, 0)).unwrap();
    assert!(
        owner
            .view_selection_gesture(b, gesture(1, press.handle, 2))
            .is_err()
    );
    let selected = owner
        .view_selection_gesture(a, gesture(1, press.handle, 2))
        .unwrap();
    assert_ne!(selected.start, 0);
    owner.predict_view_text(b, "Z".into()).unwrap();
    assert_eq!(publication.acquire_view(b).unwrap().text(), "abcZ");
    assert_eq!(publication.acquire_view(a).unwrap().text(), "abc");
    let slot = publication.view_slot(a).unwrap();
    let held = slot.acquire().unwrap();
    owner.destroy_view(a).unwrap();
    assert!(slot.acquire().is_none());
    assert_eq!(held.text(), "abc");
    assert!(owner.republish_view(a).is_err());
    assert!(owner.destroy_view(a).is_err());
    assert!(owner.has_projection(&terminal));
    assert!(owner.input_ready(&terminal));
    assert_eq!(publication.acquire_view(b).unwrap().text(), "abcZ");
    owner.clear_view_predictions(b).unwrap();
    assert_eq!(publication.acquire_view(b).unwrap().text(), "abc");
    owner.reset_connection();
    assert!(publication.acquire_view(b).is_none());
    assert!(owner.republish_view(b).is_err());
}

#[test]
fn retained_closed_views_scroll_but_document_apis_require_a_published_replica() {
    let (owner, publication) = owner();
    let terminal = id(105);
    attach(
        &owner,
        &terminal,
        b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive",
    );
    let a = owner.create_view(&terminal).unwrap();
    let b = owner.create_view(&terminal).unwrap();
    owner.scroll_view(a, Scroll::Top).unwrap();
    owner.set_retain_on_close(&terminal, true);
    apply_ok(&owner, EngineEvent::closed_unknown(terminal.clone()));
    assert_eq!(publication.acquire_view(a).unwrap().row_text(0), "zero");
    assert!(publication.acquire_view(b).unwrap().scrollbar.at_tail());
    owner.scroll_view(b, Scroll::Top).unwrap();
    owner.scroll_view(a, Scroll::Bottom).unwrap();
    assert_eq!(publication.acquire_view(b).unwrap().row_text(0), "zero");
    assert!(publication.acquire_view(a).unwrap().scrollbar.at_tail());
    assert!(owner.track_view_anchor(a, point(0, 0)).is_err());
    owner.destroy_view(a).unwrap();
    assert!(owner.has_projection(&terminal));
    owner.release(&terminal);
    assert!(!owner.has_projection(&terminal));
    assert!(publication.acquire_view(b).is_none());
    assert!(owner.republish_view(b).is_err());
}

#[test]
fn replica_replacement_keeps_view_identity_but_rejects_old_handles() {
    let (owner, publication) = owner();
    let terminal = id(106);
    attach(&owner, &terminal, b"old");
    let view = owner.create_view(&terminal).unwrap();
    let (start, end) = select(&owner, view, 0, 2);
    let held = publication.acquire_view(view).unwrap();
    let press = owner
        .view_selection_gesture(view, gesture(0, 0, 0))
        .unwrap();
    apply_ok(
        &owner,
        EngineEvent::BootstrapBegin {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(2),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
    );
    apply_ok(
        &owner,
        EngineEvent::BootstrapChunk {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(2),
            chunk_seq: 0,
            payload: b"new".to_vec().into(),
        },
    );
    apply_ok(
        &owner,
        EngineEvent::BootstrapReady {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(2),
            history_cursor: None,
        },
    );
    let frame = publication.acquire_view(view).unwrap();
    assert_eq!(frame.text(), "new");
    assert_eq!(frame.bootstrap_id, 2);
    assert!(frame.generation > held.generation);
    assert_eq!(held.text(), "old");
    assert!(owner.set_view_selection(view, start, end, false).is_err());
    assert!(
        owner
            .view_selection_gesture(view, gesture(1, press.handle, 2))
            .is_err()
    );
    assert!(owner.detach(terminal));
    assert!(publication.acquire_view(view).is_none());
}

#[test]
fn view_ids_are_not_accepted_by_another_owner() {
    let (first, _) = owner();
    let (second, _) = owner();
    let terminal = id(107);
    attach(&first, &terminal, b"a");
    attach(&second, &terminal, b"b");
    let a = first.create_view(&terminal).unwrap();
    let b = second.create_view(&terminal).unwrap();
    assert_ne!(a, b);
    assert!(first.republish_view(b).is_err());
    assert!(second.destroy_view(a).is_err());
    let a_anchor = first.track_view_anchor(a, point(0, 0)).unwrap();
    let b_anchor = second.track_view_anchor(b, point(0, 0)).unwrap();
    assert_ne!(a_anchor, b_anchor);
    assert!(
        second
            .set_view_selection(b, a_anchor, a_anchor, false)
            .is_err()
    );
}
