use super::*;
use crate::ViewId;
use phux_client_core::grid::CELL_SELECTED;

fn output(owner: &EngineHandle, terminal: &ResourceId, seq: u64, bytes: &[u8]) {
    apply_ok(
        owner,
        EngineEvent::Output {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            seq,
            bytes: bytes.to_vec(),
        },
    );
}

fn point(column: u16, row: u32) -> EngineDocumentPoint {
    EngineDocumentPoint {
        space: 1,
        column,
        row,
    }
}

fn select(owner: &EngineHandle, view: ViewId, row: u32, end: u16) -> (u64, u64) {
    let start = owner.track_view_anchor(view, point(0, row)).unwrap();
    let end = owner.track_view_anchor(view, point(end, row)).unwrap();
    owner.set_view_selection(view, start, end, false).unwrap();
    (start, end)
}

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

#[test]
fn scroll_selection_search_and_failed_callbacks_are_view_local() {
    let (owner, publication) = owner();
    let terminal = id(102);
    attach(
        &owner,
        &terminal,
        b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive",
    );
    let a = owner.create_view(&terminal).unwrap();
    let b = owner.create_view(&terminal).unwrap();
    let default = publication.acquire(&terminal).unwrap();
    let b_frame = publication.acquire_view(b).unwrap();
    owner.scroll_view(a, Scroll::Top).unwrap();
    assert_eq!(publication.acquire_view(a).unwrap().row_text(0), "zero");
    assert_eq!(publication.view_generation(b), Some(b_frame.generation));
    assert_eq!(publication.generation(&terminal), Some(default.generation));
    let (start, end) = select(&owner, a, 0, 3);
    assert_eq!(owner.view_selection_text(a).unwrap(), b"zero");
    assert!(owner.set_view_selection(b, start, end, false).is_err());
    assert!(owner.set_selection(&terminal, start, end, false).is_err());
    assert!(owner.view_selection_text(b).is_err());
    assert_ne!(
        publication
            .acquire_view(a)
            .unwrap()
            .cell(0, 0)
            .unwrap()
            .flags
            & CELL_SELECTED,
        0
    );
    assert_eq!(
        publication
            .acquire_view(b)
            .unwrap()
            .cell(0, 0)
            .unwrap()
            .flags
            & CELL_SELECTED,
        0
    );
    let matches = owner.search_view(b, "four".into(), true).unwrap();
    assert_eq!(matches.len(), 1);
    assert!(owner.pin_view(a, matches[0].start).is_err());
    assert!(owner.track_view_anchor(a, point(500, 500)).is_err());
    let default_start = owner.track_anchor(&terminal, point(0, 0)).unwrap();
    let default_end = owner.track_anchor(&terminal, point(2, 0)).unwrap();
    owner
        .set_selection(&terminal, default_start, default_end, false)
        .unwrap();
    assert_eq!(
        owner.selection_text(&terminal).unwrap(),
        b"two",
        "failed view requests restore the engine's default viewport"
    );
    owner.clear_selection(&terminal).unwrap();
    owner.release_anchor(&terminal, default_start).unwrap();
    owner.release_anchor(&terminal, default_end).unwrap();
    output(&owner, &terminal, 1, b"\r\nsix\r\nseven");
    assert_eq!(publication.acquire_view(a).unwrap().row_text(0), "zero");
    assert!(publication.acquire_view(b).unwrap().scrollbar.at_tail());
    assert!(publication.acquire(&terminal).unwrap().scrollbar.at_tail());
    assert_eq!(owner.view_selection_text(a).unwrap(), b"zero");
    assert_eq!(
        owner
            .view_replica_info(a)
            .unwrap()
            .history
            .unwrap()
            .unread_rows,
        2
    );
    assert_eq!(
        owner
            .view_replica_info(b)
            .unwrap()
            .history
            .unwrap()
            .unread_rows,
        0
    );
    assert_eq!(held_text(&default), "two\nthree\nfour\nfive");
}

#[test]
fn hyperlink_flattening_uses_the_installed_viewport() {
    let (owner, publication) = owner();
    let terminal = id(109);
    attach(&owner, &terminal, b"\x1b]8;;https://example.com/history\x1b\\head\x1b]8;;\x1b\\\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
    let history = owner.create_view(&terminal).unwrap();
    let tail = owner.create_view(&terminal).unwrap();
    owner.scroll_view(history, Scroll::Top).unwrap();
    output(&owner, &terminal, 1, b"!");
    let frame = publication.acquire_view(history).unwrap();
    let cell = frame.cell(0, 0).unwrap();
    let start = cell.hyperlink_offset as usize;
    let end = start + cell.hyperlink_len as usize;
    assert_eq!(
        &frame.buffer.utf8[start..end],
        b"https://example.com/history"
    );
    assert_eq!(
        publication
            .acquire_view(tail)
            .unwrap()
            .cell(0, 0)
            .unwrap()
            .hyperlink_len,
        0
    );
    apply_ok(&owner, EngineEvent::closed_unknown(terminal));
    assert!(publication.acquire_view(history).is_none());
    assert!(publication.acquire_view(tail).is_none());
    assert!(owner.republish_view(history).is_err());
    assert_eq!(frame.row_text(0), "head");
}

fn held_text(frame: &GridFrame) -> String {
    frame.text()
}

#[test]
fn replacing_search_preserves_selected_search_anchors() {
    let (owner, _) = owner();
    let terminal = id(103);
    attach(&owner, &terminal, b"one two one");
    let view = owner.create_view(&terminal).unwrap();
    let found = owner.search_view(view, "one".into(), true).unwrap();
    owner
        .set_view_selection(view, found[0].start, found[0].end, false)
        .unwrap();
    owner.search_view(view, "two".into(), true).unwrap();
    assert_eq!(owner.view_selection_text(view).unwrap(), b"one");
    assert!(owner.release_view_anchor(view, found[1].start).is_err());
    // Repeated searches must reclaim their allocations within the shared cap.
    for _ in 0..200 {
        owner.search_view(view, "one".into(), true).unwrap();
    }
    assert_eq!(owner.view_selection_text(view).unwrap(), b"one");
}

#[test]
fn destroying_a_view_releases_its_share_of_the_document_anchor_budget() {
    let (owner, publication) = owner();
    let terminal = id(111);
    attach(&owner, &terminal, b"text");
    let crowded = owner.create_view(&terminal).unwrap();
    let survivor = owner.create_view(&terminal).unwrap();
    let before = publication.view_generation(survivor);
    for _ in 0..config().scrollback_lines {
        owner.track_view_anchor(crowded, point(0, 0)).unwrap();
    }
    assert!(owner.track_view_anchor(survivor, point(0, 0)).is_err());
    owner.destroy_view(crowded).unwrap();
    assert!(owner.track_view_anchor(survivor, point(0, 0)).is_ok());
    assert_eq!(publication.view_generation(survivor), before);
    assert!(owner.input_ready(&terminal));
}

#[test]
fn history_tombstone_reconciles_every_view_without_waiting_for_live_output() {
    let (owner, publication) = owner();
    let terminal = id(112);
    attach_many_history(
        &owner,
        &[(&terminal, b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive")],
        (20, 4),
        Some(b"older"),
    );
    let a = owner.create_view(&terminal).unwrap();
    let b = owner.create_view(&terminal).unwrap();
    owner.scroll_view(a, Scroll::Top).unwrap();
    let (start, end) = select(&owner, a, 0, 3);
    select(&owner, b, 0, 2);
    owner.scroll(&terminal, Scroll::Top).unwrap();
    let before = publication.acquire_view(a).unwrap();
    apply_ok(
        &owner,
        EngineEvent::HistoryTombstone {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            cursor: b"older".to_vec(),
            reason: HistoryUnavailableReason::Pruned,
        },
    );
    for view in [a, b] {
        let frame = publication.acquire_view(view).unwrap();
        assert!(frame.scrollbar.at_tail());
        assert_eq!(frame.cell(0, 0).unwrap().flags & CELL_SELECTED, 0);
    }
    assert!(publication.acquire(&terminal).unwrap().scrollbar.at_tail());
    assert!(publication.view_generation(a).unwrap() > before.generation);
    assert!(owner.set_view_selection(a, start, end, false).is_err());
    assert!(owner.input_ready(&terminal));
    assert_eq!(before.row_text(0), "zero");
}

fn gesture(phase: u32, handle: u64, column: u16) -> SelectionGestureEvent {
    SelectionGestureEvent {
        phase,
        clicks: 1,
        handle,
        column,
        rectangle: false,
        row: 0,
        x: f64::from(column) * 10.0,
        y: 0.0,
        columns: 20,
        cell_width: 10,
        screen_height: 40,
        padding_left: 0,
    }
}

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
            payload: b"new".to_vec(),
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

#[test]
fn pruning_one_view_does_not_clear_another_views_recent_selection() {
    let publication = Arc::new(Publication::new());
    let mut config = config();
    config.scrollback_lines = 8;
    let owner = EngineHandle::start(&config, Arc::clone(&publication)).unwrap();
    let terminal = id(108);
    let initial = "line\r\n".repeat(20);
    attach(&owner, &terminal, initial.as_bytes());
    let old = owner.create_view(&terminal).unwrap();
    let recent = owner.create_view(&terminal).unwrap();
    owner.scroll_view(old, Scroll::Top).unwrap();
    let oldest = owner.track_view_anchor(old, point(0, 0)).unwrap();
    // Ghostty trims whole pages rather than individual rows. Exercise a real
    // page eviction while keeping a fresh selection near the active tail.
    for seq in 1..=1000 {
        let (start, end) = select(&owner, recent, 2, 3);
        let selected = owner.view_selection_text(recent).unwrap();
        output(&owner, &terminal, seq, b"next\r\nnext\r\nnext\r\nnext\r\n");
        assert_eq!(owner.view_selection_text(recent).unwrap(), selected);
        owner.clear_view_selection(recent).unwrap();
        owner.release_view_anchor(recent, start).unwrap();
        owner.release_view_anchor(recent, end).unwrap();
        if owner.pin_view(old, oldest).is_err() {
            assert!(publication.acquire_view(old).unwrap().scrollbar.at_tail());
            return;
        }
    }
    panic!("the test must observe an actual Ghostty page eviction");
}

#[test]
fn default_view_returns_to_tail_when_its_anchor_is_evicted() {
    for destroy_last_view in [false, true] {
        let publication = Arc::new(Publication::new());
        let mut config = config();
        config.scrollback_lines = 8;
        let owner = EngineHandle::start(&config, Arc::clone(&publication)).unwrap();
        let terminal = id(110);
        attach(&owner, &terminal, "line\r\n".repeat(20).as_bytes());
        if destroy_last_view {
            let view = owner.create_view(&terminal).unwrap();
            owner.destroy_view(view).unwrap();
        }
        owner.scroll(&terminal, Scroll::Top).unwrap();
        let oldest = owner.track_anchor(&terminal, point(0, 0)).unwrap();
        let mut evicted = false;
        for seq in 1..=1000 {
            output(&owner, &terminal, seq, b"next\r\nnext\r\nnext\r\nnext\r\n");
            // Capture BEFORE any query that could reconcile presentation.
            let frame = publication.acquire(&terminal).unwrap();
            if owner.pin_viewport(&terminal, oldest).is_err() {
                assert!(
                    frame.scrollbar.at_tail(),
                    "{destroy_last_view}: {:?}",
                    frame.scrollbar
                );
                evicted = true;
                break;
            }
        }
        assert!(evicted, "must exercise actual Ghostty page eviction");
    }
}

#[test]
fn saturated_search_reserves_viewport_pins_for_every_existing_view() {
    let (owner, publication) = owner();
    let terminal = id(120);
    attach(&owner, &terminal, "needle\r\n".repeat(80).as_bytes());
    let views: Vec<_> = (0..4)
        .map(|_| owner.create_view(&terminal).unwrap())
        .collect();
    let found = owner.search(&terminal, "needle".into(), true).unwrap();
    assert!(!found.is_empty());
    for view in &views {
        owner.scroll_view(*view, Scroll::Top).unwrap();
        assert_eq!(
            publication.acquire_view(*view).unwrap().row_text(0),
            "needle"
        );
    }
    for matched in found.iter().take(3) {
        owner.pin_viewport(&terminal, matched.start).unwrap();
        for view in &views {
            owner.scroll_view(*view, Scroll::Delta(1)).unwrap();
        }
    }
    // A search scoped to a view must also preserve headroom in the shared
    // terminal budget, including the temporarily swapped default state.
    owner.clear_search(&terminal).unwrap();
    owner.scroll(&terminal, Scroll::Bottom).unwrap();
    owner.scroll_view(views[1], Scroll::Bottom).unwrap();
    let found = owner.search_view(views[0], "needle".into(), true).unwrap();
    assert!(!found.is_empty());
    owner.scroll(&terminal, Scroll::Top).unwrap();
    owner.scroll_view(views[1], Scroll::Top).unwrap();
    for matched in found.iter().take(3) {
        owner.pin_view(views[0], matched.start).unwrap();
        owner.scroll(&terminal, Scroll::Delta(1)).unwrap();
        owner.scroll_view(views[1], Scroll::Delta(1)).unwrap();
    }
}

#[test]
fn failed_reveal_preserves_the_physical_and_published_viewport() {
    let (owner, publication) = owner();
    let terminal = id(121);
    attach(
        &owner,
        &terminal,
        b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive",
    );
    owner.scroll(&terminal, Scroll::Top).unwrap();
    let target = owner.track_anchor(&terminal, point(0, 0)).unwrap();
    owner.scroll(&terminal, Scroll::Row(1)).unwrap();
    let before = publication.acquire(&terminal).unwrap();
    assert_eq!(before.row_text(0), "one");
    let mut held = Vec::new();
    while let Ok(anchor) = owner.track_anchor(&terminal, point(0, 0)) {
        held.push(anchor);
    }
    assert!(owner.pin_viewport(&terminal, target).is_err());
    let after = publication.acquire(&terminal).unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.row_text(0), "one");

    // Read the actual engine viewport, not only the last published frame.
    owner
        .release_anchor(&terminal, held.pop().unwrap())
        .unwrap();
    let current = owner.track_anchor(&terminal, point(0, 0)).unwrap();
    owner
        .set_selection(&terminal, current, current, false)
        .unwrap();
    assert_eq!(owner.selection_text(&terminal).unwrap(), b"o");
}

#[test]
fn prompt_jumps_select_all_and_region_text_stay_in_one_view() {
    let (owner, publication) = owner();
    let terminal = id(131);
    attach(&owner, &terminal, b"");
    let a = owner.create_view(&terminal).unwrap();
    let b = owner.create_view(&terminal).unwrap();
    // Rows 0, 3 and 6 carry OSC 133 prompts; the 4-row viewport shows 3..=6.
    output(
        &owner,
        &terminal,
        1,
        b"\x1b]133;A\x07$ one\r\nout1\r\nout1b\r\n\x1b]133;A\x07$ two\r\nout2\r\nout2b\r\n\x1b]133;A\x07$ three",
    );
    let b_generation = publication.view_generation(b).unwrap();
    let top = |view| publication.acquire_view(view).unwrap().row_text(0);
    assert_eq!(top(a), "$ two");

    owner.scroll_view(a, Scroll::Prompt(-1)).unwrap();
    assert_eq!(top(a), "$ one");
    // No prompt above row 0: the viewport stays.
    owner.scroll_view(a, Scroll::Prompt(-1)).unwrap();
    assert_eq!(top(a), "$ one");
    // The next prompt is in the active area, so the view follows the tail.
    owner.scroll_view(a, Scroll::Prompt(1)).unwrap();
    assert!(publication.acquire_view(a).unwrap().scrollbar.at_tail());
    // Fewer prompts than asked for: the farthest one found.
    owner.scroll_view(a, Scroll::Prompt(-5)).unwrap();
    assert_eq!(top(a), "$ one");
    owner.scroll_view(a, Scroll::Prompt(0)).unwrap();
    assert_eq!(top(a), "$ one");
    assert_eq!(publication.view_generation(b), Some(b_generation));

    assert!(owner.select_all_view(a).unwrap());
    assert_eq!(
        String::from_utf8(owner.view_selection_text(a).unwrap()).unwrap(),
        "$ one\nout1\nout1b\n$ two\nout2\nout2b\n$ three"
    );
    assert!(owner.view_selection_text(b).is_err());

    let region = |region, max| owner.view_region_text_bounded(b, region, max).unwrap();
    assert_eq!(
        region(TextRegion::History, 1024),
        BoundedSelectionText::Text(b"$ one\nout1\nout1b".to_vec())
    );
    assert_eq!(
        region(TextRegion::Screen, 1024),
        BoundedSelectionText::Text(b"$ one\nout1\nout1b\n$ two\nout2\nout2b\n$ three".to_vec())
    );
    assert_eq!(
        region(TextRegion::Screen, 8),
        BoundedSelectionText::ByteLimitExceeded
    );
    // Reading a region neither selects nor republishes the reading view.
    assert!(owner.view_selection_text(b).is_err());
    assert_eq!(publication.view_generation(b), Some(b_generation));
}
