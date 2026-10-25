use super::*;
#[test]
fn warm_views_share_output_without_dirty_reader_starvation() {
    let (owner, publication) = owner();
    let terminal = id(101);
    attach(&owner, &terminal, b"first\r\nsecond");
    let a = owner.create_view(&terminal).unwrap();
    let b = owner.create_view(&terminal).unwrap();
    for view in [a, b] {
        owner.republish_view(view).unwrap();
    }
    let held = publication.acquire_view(a).unwrap();
    let a_generation = held.generation;
    let b_generation = publication.view_generation(b).unwrap();
    output(&owner, &terminal, 1, b"\x1b[1;1HX");
    for (view, generation) in [(a, a_generation), (b, b_generation)] {
        let frame = publication.acquire_view(view).unwrap();
        assert_eq!(frame.row_text(0), "Xirst");
        assert_eq!(frame.generation, generation + 1);
        assert_eq!(frame.damage, GridDamage::Full);
        assert_eq!(frame.dirty_rows().collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert_eq!((frame.cols, frame.rows), (20, 4));
        assert_eq!(frame.terminal_id, terminal);
    }
    assert_eq!(publication.acquire(&terminal).unwrap().row_text(0), "Xirst");
    assert_eq!(held.row_text(0), "first");
}

#[test]
fn cell_pixel_changes_republish_quiet_image_views() {
    let (owner, publication) = owner();
    let terminal = id(102);
    attach(
        &owner,
        &terminal,
        b"\x1b_Ga=T,f=24,s=1,v=1,i=42,C=1;/wAA\x1b\\",
    );
    let views = [
        owner.create_view(&terminal).unwrap(),
        owner.create_view(&terminal).unwrap(),
    ];
    let before: Vec<_> = views
        .iter()
        .map(|view| publication.acquire_view(*view).unwrap())
        .collect();
    assert_eq!(
        (before[0].images[0].cols, before[0].images[0].rows),
        (0.125, 0.0625)
    );
    owner.set_cell_pixels(&terminal, 16, 32).unwrap();
    for (view, previous) in views.into_iter().zip(before) {
        let frame = publication.acquire_view(view).unwrap();
        assert!(frame.generation > previous.generation);
        assert_eq!(
            (frame.images[0].cols, frame.images[0].rows),
            (0.0625, 0.03125)
        );
        assert!(Arc::ptr_eq(
            &frame.images[0].image,
            &previous.images[0].image
        ));
        owner.set_cell_pixels(&terminal, 16, 32).unwrap();
        assert_eq!(
            publication.acquire_view(view).unwrap().generation,
            frame.generation
        );
    }
}

#[test]
fn screen_swaps_retire_only_affected_presentations_and_publish_every_view() {
    let (owner, publication) = owner();
    let terminal = id(111);
    let other = id(112);
    attach_many(
        &owner,
        &[
            (&terminal, b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive"),
            (&other, b"sibling"),
        ],
    );
    let left = owner.create_view(&terminal).unwrap();
    let right = owner.create_view(&terminal).unwrap();
    let sibling = owner.create_view(&other).unwrap();
    let (old, _) = select(&owner, right, 2, 3);
    let found = owner.search_view(right, "zero".into(), true).unwrap();
    owner.pin_view(right, found[0].start).unwrap();
    select(&owner, sibling, 0, 3);
    let sibling_frame = publication.acquire_view(sibling).unwrap();
    let held = publication.acquire_view(right).unwrap();
    output(&owner, &terminal, 1, b"\x1b[?1049h\x1b[HALT");
    for view in [left, right] {
        let frame = publication.acquire_view(view).unwrap();
        assert_eq!(frame.last_seq, 1);
        assert_eq!(frame.row_text(0), "ALT");
        assert!(frame.scrollbar.at_tail());
        assert!(
            frame
                .buffer
                .cells
                .iter()
                .all(|cell| cell.flags & CELL_SELECTED == 0)
        );
    }
    assert!(owner.pin_view(right, old).is_err());
    assert!(owner.pin_view(right, found[0].start).is_err());
    let (alt, _) = select(&owner, right, 0, 2);
    let alt_search = owner.search_view(right, "ALT".into(), true).unwrap();
    output(&owner, &terminal, 2, b"\x1b[?1049l");
    for view in [left, right] {
        let frame = publication.acquire_view(view).unwrap();
        assert_eq!(frame.last_seq, 2);
        assert_eq!(frame.row_text(0), "two");
        assert!(frame.scrollbar.at_tail());
        assert!(
            frame
                .buffer
                .cells
                .iter()
                .all(|cell| cell.flags & CELL_SELECTED == 0)
        );
    }
    assert!(owner.pin_view(right, alt).is_err());
    assert!(owner.pin_view(right, alt_search[0].start).is_err());
    assert_eq!(
        publication.view_generation(sibling),
        Some(sibling_frame.generation)
    );
    assert_eq!(owner.view_selection_text(sibling).unwrap(), b"sibl");
    assert_eq!(held.row_text(0), "zero");
}

/// The desktop's shape: the window reads a view and nobody reads the
/// terminal's default presentation. The view publishes every batch; the
/// default projects once, when something finally reads it.
#[test]
fn an_unread_default_is_deferred_while_a_read_view_publishes_every_batch() {
    let (owner, publication) = owner();
    let terminal = id(131);
    attach(&owner, &terminal, b"first\r\nsecond");
    let view = owner.create_view(&terminal).unwrap();
    let default = publication.generation(&terminal).unwrap();
    for seq in 1..=3 {
        let generation = publication.acquire_view(view).unwrap().generation;
        output(&owner, &terminal, seq, format!("\x1b[1;1H{seq}").as_bytes());
        let frame = publication.acquire_view(view).unwrap();
        assert_eq!(frame.generation, generation + 1);
        assert_eq!(frame.damage, GridDamage::Rows);
        assert!(frame.is_row_dirty(0), "live output row must repaint");
        assert!(
            frame.dirty_rows().count() < usize::from(frame.rows),
            "an unchanged row must not be republished"
        );
    }
    assert_eq!(publication.generation(&terminal), Some(default));

    let caught_up = publication.acquire(&terminal).unwrap();
    assert_eq!(caught_up.generation, default + 1);
    assert_eq!(caught_up.row_text(0), "3irst");
    assert_eq!(caught_up.damage, GridDamage::Full);
}

/// Row damage is relative to the projector's previous projection. Once a
/// default presentation has fallen behind a view, its next frame cannot be
/// incremental, even after the view is gone.
#[test]
fn a_presentation_behind_another_reader_republishes_in_full() {
    let (owner, publication) = owner();
    let terminal = id(141);
    attach(&owner, &terminal, b"first\r\nsecond\r\nthird");
    let view = owner.create_view(&terminal).unwrap();
    let _ = publication.acquire_view(view).unwrap();
    output(&owner, &terminal, 1, b"\x1b[1;1HX");
    owner.destroy_view(view).unwrap();
    output(&owner, &terminal, 2, b"\x1b[3;1HY");

    let frame = publication.acquire(&terminal).unwrap();
    assert_eq!(frame.row_text(0), "Xirst");
    assert_eq!(frame.row_text(2), "Yhird");
    assert_eq!(frame.damage, GridDamage::Full);
    assert_eq!(frame.dirty_rows().count(), usize::from(frame.rows));
}

/// A painter's one-turn read: a deferred view is caught up inside the same
/// owner turn that reports its replica, so the two always agree, and the
/// handed-off frame counts as read so the next batch publishes eagerly.
#[test]
fn present_view_catches_up_and_agrees_with_its_replica_in_one_turn() {
    let (owner, publication) = owner();
    let terminal = id(151);
    attach(&owner, &terminal, b"first\r\nsecond");
    let view = owner.create_view(&terminal).unwrap();
    // Unread since creation: these batches defer the view's projection.
    let before = publication.view_generation(view).unwrap();
    output(&owner, &terminal, 1, b"\x1b[1;1HA");
    output(&owner, &terminal, 2, b"\x1b[1;1HB");
    assert_eq!(publication.view_generation(view), Some(before));

    let presented = owner.present_view(view).unwrap();
    let frame = presented.frame.expect("published view");
    assert_eq!(frame.row_text(0), "Birst");
    assert_eq!(presented.replica.last_seq, 2);
    assert_eq!(
        (frame.stream_id, frame.bootstrap_id, frame.last_seq),
        (
            presented.replica.stream_id,
            presented.replica.bootstrap_id,
            presented.replica.last_seq
        )
    );
    assert!(presented.replica.input_ready);
    assert_eq!(publication.view_generation(view), Some(frame.generation));

    // Read: the next batch publishes without a catch-up.
    output(&owner, &terminal, 3, b"\x1b[1;1HC");
    assert_eq!(
        publication.view_generation(view),
        Some(frame.generation + 1)
    );
    owner.destroy_view(view).unwrap();
    assert!(owner.present_view(view).is_err());
}

/// The replica's own OSC 133 marks say whether a command has the screen,
/// with no shell event on this connection.
#[test]
fn the_replica_reads_its_prompt_cycle_from_osc_133_marks() {
    let (owner, _publication) = owner();
    let at_prompt = id(201);
    let running = id(202);
    let scrolled = id(203);
    let plain = id(204);
    let full_screen = id(205);
    let mut long = b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07".to_vec();
    for line in 0..12 {
        long.extend_from_slice(format!("cc {line}\r\n").as_bytes());
    }
    attach_many(
        &owner,
        &[
            (
                &at_prompt,
                b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a b\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07",
            ),
            (
                &running,
                b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07building",
            ),
            (&scrolled, &long),
            (&plain, b"$ make\r\nbuilding"),
            (
                &full_screen,
                b"\x1b]133;A\x07$ \x1b]133;B\x07vim\r\n\x1b]133;C\x07\x1b[?1049h~",
            ),
        ],
    );
    assert_eq!(owner.replica_prompt(&at_prompt), ReplicaPrompt::AtPrompt);
    assert_eq!(owner.replica_prompt(&running), ReplicaPrompt::Running);
    assert_eq!(
        owner.replica_prompt(&scrolled),
        ReplicaPrompt::Running,
        "a prompt scrolled into history still counts"
    );
    assert_eq!(owner.replica_prompt(&plain), ReplicaPrompt::Unknown);
    assert_eq!(owner.replica_prompt(&full_screen), ReplicaPrompt::Unknown);
    assert_eq!(owner.replica_prompt(&id(299)), ReplicaPrompt::Unknown);
}
