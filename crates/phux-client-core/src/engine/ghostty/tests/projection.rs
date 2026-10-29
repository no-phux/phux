//! History projection bounds, search, and anchors.

use super::*;

#[test]
fn native_history_bounds_every_projection_under_page_granular_storage() {
    let captured = capture_snapshot();
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, &captured.ready, &mut effects)
        .expect("checkpoint through READY");
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .expect("protocol READY");
    adapter
        .configure_history_budget(&mut replica, 64 * 1024, 2)
        .expect("engine history limits");
    let mut physical_high_water = 0;
    let mut authenticated_rows = 0;
    let mut retained_rows = 0;
    let mut discarded_page = false;
    for (page, rows) in &captured.history {
        let outcome = adapter
            .apply_history_page(&mut replica, page, *rows, &mut effects)
            .expect("bounded history unit");
        authenticated_rows += outcome.authenticated_rows;
        retained_rows += outcome.retained_rows;
        discarded_page |= outcome.authenticated_rows != 0 && outcome.retained_rows == 0;
        physical_high_water = physical_high_water.max(
            replica
                .terminal()
                .expect("live terminal")
                .scrollback_rows()
                .expect("scrollback rows"),
        );
        let projection = adapter
            .project_history(&mut replica, 80, EngineProjectionOrigin::Tail, 2)
            .expect("bounded projection");
        assert!(projection.rows.len() <= 2);
    }
    assert!(physical_high_water <= crate::history::MAX_HISTORY_PAGE_ROWS as usize);
    assert!(discarded_page);
    assert!(authenticated_rows > retained_rows);
}

#[test]
fn search_case_policy_preserves_unicode_and_original_document_points() {
    let needle: Vec<char> = "éx".chars().collect();
    let start = history_point(7, 4);
    let end = history_point(8, 4);
    let mut insensitive = NeedleScan::new(&needle, 2, false);
    insensitive.push('é', start);
    insensitive.push('X', end);
    insensitive.push('É', history_point(9, 4));
    insensitive.push('x', history_point(10, 4));
    assert_eq!(insensitive.into_ranges(), vec![(start, end)]);

    let mut sensitive = NeedleScan::new(&needle, 2, true);
    sensitive.push('é', start);
    sensitive.push('X', end);
    assert!(sensitive.into_ranges().is_empty());
}

#[test]
fn search_keeps_sparse_cells_at_soft_wraps_but_not_hard_line_padding() {
    let mut terminal = GhosttyTerminal::new(4, 4).expect("terminal");
    terminal.vt_write("a\x1b[2C界x".as_bytes());
    assert_eq!(history_row_wrapped_at(&terminal, 0).unwrap(), Some(true));
    assert!(
        scan_history_for_needle(&terminal, "a界x", 1, true)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        scan_history_for_needle(&terminal, "a  界x", 1, true).unwrap(),
        vec![(history_point(0, 0), history_point(2, 1))]
    );

    let mut terminal = GhosttyTerminal::new(4, 4).expect("terminal");
    terminal.vt_write(b"ab\r\ncd");
    assert_eq!(
        scan_history_for_needle(&terminal, "ab\ncd", 1, true).unwrap(),
        vec![(history_point(0, 0), history_point(1, 1))]
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_projection_search_and_anchors_remain_engine_owned() {
    let captured = capture_snapshot();
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, &captured.ready, &mut effects)
        .expect("checkpoint through READY");
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .expect("protocol READY");
    apply_history(&mut adapter, &mut replica, &captured, &mut effects);

    let projection = adapter
        .project_history(&mut replica, 12, EngineProjectionOrigin::Tail, 3)
        .expect("bounded projection");
    assert_eq!(projection.width, 12);
    assert_eq!(projection.rows.len(), 3);
    assert!(projection.has_older);
    let narrow = adapter
        .project_history(&mut replica, 1, EngineProjectionOrigin::Tail, 3)
        .expect("minimum-width projection");
    assert_eq!(narrow.width, 2);
    assert!(
        narrow
            .rows
            .iter()
            .all(|row| row.text.chars().count() <= usize::from(narrow.width))
    );

    let found = adapter
        .search_loaded(&mut replica, "line 2990", 1)
        .expect("engine search");
    assert_eq!(found.len(), 1);
    let selection = EngineDocumentSelection {
        start: found[0].start,
        end: found[0].end,
        rectangle: false,
    };
    assert_eq!(
        adapter
            .format_selection(&replica, selection)
            .expect("engine formatting")
            .as_deref(),
        Some("line 2990")
    );
    let full_scrollback = replica
        .terminal()
        .expect("live terminal")
        .scrollback_rows()
        .expect("scrollback rows");
    adapter
        .configure_history_budget(&mut replica, 64 * 1024, full_scrollback)
        .expect("full scrollback cap");
    let distance_before = adapter
        .history_anchor_tail_distance(&replica, found[0].start)
        .expect("anchor distance")
        .expect("live anchor");
    adapter
        .apply_output(&mut replica, b"newer-live-output\r\n", &mut effects)
        .expect("live append");
    let distance_after = adapter
        .history_anchor_tail_distance(&replica, found[0].start)
        .expect("anchor distance")
        .expect("retained anchor");
    assert_eq!(distance_after, distance_before + 1);
    adapter
        .apply_output(&mut replica, b"\x1b]2;control-only\x07", &mut effects)
        .expect("control-only output");
    assert_eq!(
        adapter
            .history_anchor_tail_distance(&replica, found[0].start)
            .expect("anchor distance")
            .expect("retained anchor"),
        distance_after
    );
    adapter
        .apply_output(
            &mut replica,
            b"coalesced-one\r\ncoalesced-two\r\n",
            &mut effects,
        )
        .expect("coalesced output");
    assert_eq!(
        adapter
            .history_anchor_tail_distance(&replica, found[0].start)
            .expect("anchor distance")
            .expect("retained anchor"),
        distance_after + 2
    );
    assert!(
        adapter
            .document_anchor_point(&replica, found[0].start, DocumentSpace::History)
            .expect("tracked point")
            .is_some()
    );

    let other = adapter
        .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
        .expect("other replica");
    assert_eq!(
        adapter
            .document_anchor_point(&other, found[0].start, DocumentSpace::History)
            .expect("wrong replica lookup"),
        None
    );
    adapter.release_document_anchor(&mut replica, found[0].start);
    adapter.release_document_anchor(&mut replica, found[0].end);
    assert!(replica.anchors.is_empty());
    assert!(other.anchors.is_empty());
}

#[test]
fn projection_extends_anchor_to_complete_soft_wrapped_line() {
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
        .expect("synthesized replica");
    let mut effects = EngineEffectBuffer::new();
    let mut output = vec![b'x'; 320];
    output.extend_from_slice(b"\r\none\r\ntwo\r\nthree\r\nfour\r\n");
    adapter
        .apply_output(&mut replica, &output, &mut effects)
        .expect("wrapped output");
    let anchor = adapter
        .track_document_anchor(
            &mut replica,
            DocumentPoint {
                space: DocumentSpace::History,
                x: 1,
                y: 2,
            },
        )
        .expect("anchor inside wrapped line");
    let projection = adapter
        .project_history(&mut replica, 320, EngineProjectionOrigin::Anchor(anchor), 1)
        .expect("complete logical line");
    assert_eq!(projection.width, 320);
    assert_eq!(projection.rows.len(), 1);
    assert_eq!(projection.rows[0].text, "x".repeat(320));
    assert!(!projection.has_older);
}

fn projected(text: &str, width: usize) -> ProjectedCell {
    ProjectedCell {
        text: text.to_owned(),
        width,
        empty_default: false,
    }
}

#[test]
fn projection_trims_only_trailing_default_cells() {
    let rows = rewrap_history_rows(
        vec![(
            vec![
                projected("a", 1),
                ProjectedCell {
                    text: String::new(),
                    width: 1,
                    empty_default: true,
                },
                projected("b", 1),
                ProjectedCell {
                    text: String::new(),
                    width: 1,
                    empty_default: true,
                },
            ],
            false,
        )],
        8,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text, "a b");
    assert!(!rows[0].soft_wrapped);
}

#[test]
fn projection_joins_soft_rows_and_keeps_wide_graphemes_atomic() {
    let rows = rewrap_history_rows(
        vec![
            (
                vec![
                    projected("a", 1),
                    projected("e\u{301}", 1),
                    projected("界", 2),
                ],
                true,
            ),
            (vec![projected("z", 1)], false),
        ],
        3,
    );
    assert_eq!(
        rows.iter()
            .map(|row| (row.text.as_str(), row.soft_wrapped))
            .collect::<Vec<_>>(),
        vec![("ae\u{301}", true), ("界z", false)]
    );
}

#[test]
fn minimum_projection_width_keeps_leading_cjk_within_row() {
    let rows = rewrap_history_rows(
        vec![(vec![projected("界", 2), projected("a", 1)], false)],
        2,
    );
    assert_eq!(
        rows.iter()
            .map(|row| (row.text.as_str(), row.soft_wrapped))
            .collect::<Vec<_>>(),
        vec![("界", true), ("a", false)]
    );
}
