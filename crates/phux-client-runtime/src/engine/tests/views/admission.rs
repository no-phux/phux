//! Source identity is checked at the actual owner admission, not at capture.

use super::*;
use crate::engine::views::ViewCommand;
use crate::publication::TerminalPublication;

/// A captured source from a retired incarnation is refused at admission.
fn assert_stale(owner: &EngineHandle, source: &ViewSource) {
    assert!(matches!(
        owner.create_view(source),
        Err(EngineError::StaleViewSource)
    ));
}

/// Every slot of a retired presentation reads as gone.
fn assert_retired(slots: &[&TerminalPublication]) {
    for slot in slots {
        assert!(slot.acquire().is_none());
    }
}

#[test]
fn reset_reusing_resource_stream_and_bootstrap_rejects_the_original_source() {
    let (owner, publication) = owner();
    let terminal = id(301);
    attach(&owner, &terminal, b"old");
    let original = owner.view_source(&terminal).unwrap();
    assert_eq!(original.terminal_id(), &terminal);
    let old_view = owner.create_view(&original).unwrap();
    let old_slot = publication.view_slot(old_view).unwrap();
    let old_default = publication.slot(&terminal).unwrap();
    let held = old_slot.acquire().unwrap();

    owner.reset_connection();
    assert_stale(&owner, &original);
    assert_eq!(
        owner
            .request(|reply| Command::Query(Query::ViewCount(reply)))
            .unwrap(),
        0
    );
    assert!(publication.slot(&terminal).is_none());
    assert_retired(&[&old_slot, &old_default]);

    // Deliberately reuse every wire-visible identity. Only the publication
    // slot distinguishes this incarnation from the captured source.
    attach(
        &owner,
        &terminal,
        b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive",
    );
    let fresh = owner.view_source(&terminal).unwrap();
    let left = owner.create_view(&fresh).unwrap();
    let right = owner.create_view(&fresh).unwrap();
    owner.scroll_view(left, Scroll::Top).unwrap();
    select(&owner, left, 0, 3);
    let default = publication.acquire(&terminal).unwrap();
    let left_frame = publication.acquire_view(left).unwrap();
    let right_frame = publication.acquire_view(right).unwrap();
    assert_eq!((default.stream_id, default.bootstrap_id), (1, 1));

    assert_stale(&owner, &original);
    assert!(Arc::ptr_eq(
        &default,
        &publication.acquire(&terminal).unwrap()
    ));
    assert!(Arc::ptr_eq(
        &left_frame,
        &publication.acquire_view(left).unwrap()
    ));
    assert!(Arc::ptr_eq(
        &right_frame,
        &publication.acquire_view(right).unwrap()
    ));
    assert_eq!(owner.view_selection_text(left).unwrap(), b"zero");
    assert!(right_frame.scrollbar.at_tail());
    assert!(owner.input_ready(&terminal));
    assert_eq!(
        owner
            .request(|reply| Command::Query(Query::ViewCount(reply)))
            .unwrap(),
        2
    );
    assert_retired(&[&old_slot, &old_default]);
    assert_eq!(held.text(), "old");

    // Explicit fresh intent remains usable; closing only that presentation
    // must not retire either independent sibling or the execution.
    let admitted = owner.create_view(&fresh).unwrap();
    owner.destroy_view(admitted).unwrap();
    owner.destroy_view(left).unwrap();
    assert!(publication.acquire_view(right).unwrap().scrollbar.at_tail());
    assert!(owner.input_ready(&terminal));
    assert!(owner.republish_view(old_view).is_err());
}

#[test]
fn replacement_rejects_delayed_creation_without_retiring_existing_views() {
    let (owner, publication) = owner();
    let terminal = id(302);
    let other = id(303);
    attach_many(
        &owner,
        &[
            (&terminal, b"old"),
            (&other, b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive"),
        ],
    );
    let original = owner.view_source(&terminal).unwrap();
    let existing = owner.create_view(&original).unwrap();
    let other_source = owner.view_source(&other).unwrap();
    let sibling = owner.create_view(&other_source).unwrap();
    owner.scroll_view(sibling, Scroll::Top).unwrap();
    select(&owner, sibling, 0, 3);
    rebootstrap(&owner, &terminal, 2, b"replacement");
    let existing_frame = publication.acquire_view(existing).unwrap();
    let sibling_frame = publication.acquire_view(sibling).unwrap();
    let default = publication.acquire(&terminal).unwrap();

    assert_stale(&owner, &original);
    assert_eq!(
        owner
            .request(|reply| Command::Query(Query::ViewCount(reply)))
            .unwrap(),
        2
    );
    assert!(Arc::ptr_eq(
        &default,
        &publication.acquire(&terminal).unwrap()
    ));
    assert!(Arc::ptr_eq(
        &existing_frame,
        &publication.acquire_view(existing).unwrap()
    ));
    assert!(Arc::ptr_eq(
        &sibling_frame,
        &publication.acquire_view(sibling).unwrap()
    ));
    assert_eq!(existing_frame.text(), "replacement");
    assert_eq!(owner.view_selection_text(sibling).unwrap(), b"zero");
    let fresh = owner.view_source(&terminal).unwrap();
    let admitted = owner.create_view(&fresh).unwrap();
    owner.destroy_view(existing).unwrap();
    assert_eq!(
        publication.acquire_view(admitted).unwrap().text(),
        "replacement"
    );
    assert_eq!(owner.view_selection_text(sibling).unwrap(), b"zero");
    assert!(owner.input_ready(&terminal));
    assert!(owner.input_ready(&other));
    assert_eq!(
        owner
            .request(|reply| Command::Query(Query::ViewCount(reply)))
            .unwrap(),
        2
    );
}

#[test]
fn queued_create_checks_source_after_reset_and_replacement_at_owner_admission() {
    let (owner, publication) = owner();
    let terminal = id(304);
    attach(&owner, &terminal, b"old");
    let original = owner.view_source(&terminal).unwrap();
    let original_slot = publication.slot(&terminal).unwrap();
    let held = original_slot.acquire().unwrap();

    let (entered, resume) = owner.pause_next_apply_for_test().unwrap();
    let (paused_reply, paused) = mpsc::channel();
    Command::ApplyBatch(Vec::new(), paused_reply)
        .send(&owner.commands)
        .unwrap();
    entered.recv().unwrap();

    // The owner is paused with the original source still current. Queue a
    // reset and replacement ahead of Create, bypassing any handle-side check.
    let (reset_reply, reset) = mpsc::channel();
    Command::Lifecycle(Lifecycle::Reset(reset_reply))
        .send(&owner.commands)
        .unwrap();
    let (replacement_reply, replacement) = mpsc::channel();
    Command::ApplyBatch(
        bootstrap_events(&terminal, 1, (20, 4), b"replacement", None).into(),
        replacement_reply,
    )
    .send(&owner.commands)
    .unwrap();
    let (create_reply, create) = mpsc::channel();
    Command::View(ViewCommand::Create(original.clone(), create_reply))
        .send(&owner.commands)
        .unwrap();
    resume.send(()).unwrap();
    assert!(paused.recv().unwrap().is_empty());
    reset.recv().unwrap();
    let outcomes = replacement.recv().unwrap();
    assert!(outcomes.iter().all(|outcome| outcome.error.is_none()));
    assert!(matches!(
        create.recv().unwrap(),
        Err(EngineError::StaleViewSource)
    ));
    assert_eq!(
        owner
            .request(|reply| Command::Query(Query::ViewCount(reply)))
            .unwrap(),
        0
    );
    assert!(original_slot.acquire().is_none());
    assert_eq!(held.text(), "old");
    assert_eq!(
        publication.acquire(&terminal).unwrap().text(),
        "replacement"
    );

    let fresh = owner.view_source(&terminal).unwrap();
    let view = owner.create_view(&fresh).unwrap();
    assert_eq!(
        publication.acquire_view(view).unwrap().text(),
        "replacement"
    );
    assert!(matches!(
        owner.create_view(&original),
        Err(EngineError::StaleViewSource)
    ));
    owner.destroy_view(view).unwrap();
    assert!(owner.input_ready(&terminal));
}

#[test]
fn another_owner_cannot_admit_a_source_with_identical_wire_ids() {
    let (first, _) = owner();
    let (second, publication) = owner();
    let terminal = id(305);
    attach(&first, &terminal, b"first");
    attach(&second, &terminal, b"second");
    let source = first.view_source(&terminal).unwrap();
    let before = publication.acquire(&terminal).unwrap();
    assert!(matches!(
        second.create_view(&source),
        Err(EngineError::StaleViewSource)
    ));
    assert_eq!(
        second
            .request(|reply| Command::Query(Query::ViewCount(reply)))
            .unwrap(),
        0
    );
    assert!(Arc::ptr_eq(
        &before,
        &publication.acquire(&terminal).unwrap()
    ));
    let own_source = second.view_source(&terminal).unwrap();
    let view = second.create_view(&own_source).unwrap();
    assert_eq!(publication.acquire_view(view).unwrap().text(), "second");
}

#[test]
fn ordinary_output_does_not_replace_the_captured_stream_source() {
    let (owner, publication) = owner();
    let terminal = id(306);
    attach(&owner, &terminal, b"first");
    let source = owner.view_source(&terminal).unwrap();
    let before = publication.acquire(&terminal).unwrap();
    output(&owner, &terminal, 1, b"!");
    let view = owner.create_view(&source).unwrap();
    let frame = publication.acquire_view(view).unwrap();
    assert_eq!(frame.text(), "first!");
    assert_eq!(
        (frame.stream_id, frame.bootstrap_id),
        (before.stream_id, before.bootstrap_id)
    );
    assert!(publication.acquire(&terminal).unwrap().generation > before.generation);
}
