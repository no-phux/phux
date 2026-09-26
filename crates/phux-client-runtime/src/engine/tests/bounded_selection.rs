use super::*;

fn select_range(
    owner: &EngineHandle,
    view: crate::ViewId,
    start: (u16, u32),
    end: (u16, u32),
    rectangle: bool,
) {
    let start = owner
        .track_view_anchor(
            view,
            EngineDocumentPoint {
                space: 0,
                column: start.0,
                row: start.1,
            },
        )
        .unwrap();
    let end = owner
        .track_view_anchor(
            view,
            EngineDocumentPoint {
                space: 0,
                column: end.0,
                row: end.1,
            },
        )
        .unwrap();
    owner
        .set_view_selection(view, start, end, rectangle)
        .unwrap();
}

#[test]
fn bounded_copy_matches_legacy_utf8_wrap_trim_and_rectangle_semantics() {
    let (owner, _) = owner();
    let terminal = id(201);
    attach(
        &owner,
        &terminal,
        "é界e\u{301} -- wrapped text past column twenty\r\nnext  ".as_bytes(),
    );
    let view = owner.create_view(&terminal).unwrap();
    assert_eq!(
        owner.selection_text_view_bounded(view, 10).unwrap(),
        BoundedSelectionText::Unavailable
    );
    for rectangle in [false, true] {
        select_range(&owner, view, (3, 2), (0, 0), rectangle);
        let expected = owner.view_selection_text(view).unwrap();
        assert!(std::str::from_utf8(&expected).unwrap().contains('é'));
        assert_eq!(
            owner
                .selection_text_view_bounded(view, expected.len())
                .unwrap(),
            BoundedSelectionText::Text(expected.clone())
        );
        assert_eq!(
            owner
                .selection_text_view_bounded(view, expected.len() - 1)
                .unwrap(),
            BoundedSelectionText::ByteLimitExceeded
        );
        assert_eq!(
            owner.selection_text_view_bounded(view, 1).unwrap(),
            BoundedSelectionText::ByteLimitExceeded,
            "never return a partial UTF-8 scalar"
        );
        assert_eq!(
            owner.selection_text_view_bounded(view, 0).unwrap(),
            BoundedSelectionText::ByteLimitExceeded
        );
        owner.clear_view_selection(view).unwrap();
    }
    select_range(&owner, view, (10, 3), (12, 3), false);
    assert_eq!(
        owner.selection_text_view_bounded(view, 0).unwrap(),
        BoundedSelectionText::Text(Vec::new())
    );
    assert!(
        owner.selection_text_view_bounded(view, usize::MAX).is_err(),
        "overflowing budgets cannot allocate"
    );
    owner.destroy_view(view).unwrap();
    assert!(owner.selection_text_view_bounded(view, 1024).is_err());
}

#[test]
fn several_megabytes_are_refused_before_formatting_and_sibling_stays_intact() {
    let publication = Arc::new(Publication::new());
    let mut config = config();
    config.history = Some(HistoryCacheConfig {
        max_bytes: 128 * 1024 * 1024,
        max_materialized_rows: 40_000,
        ..HistoryCacheConfig::default()
    });
    let owner = EngineHandle::start(&config, Arc::clone(&publication)).unwrap();
    let terminal = id(202);
    attach_many_sized(&owner, &[(&terminal, b"")], (120, 4));
    let bytes = format!("{}\r\n", "é".repeat(119))
        .repeat(30_000)
        .into_bytes();
    apply_ok(
        &owner,
        EngineEvent::Output {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            seq: 1,
            bytes,
        },
    );
    let huge = owner.create_view(&terminal).unwrap();
    let sibling = owner.create_view(&terminal).unwrap();
    let bar = publication.acquire_view(huge).unwrap().scrollbar;
    assert!(
        bar.total > 29_000,
        "test must retain several MiB of real history"
    );
    let last = u32::try_from(bar.total - 2).unwrap();
    select_range(&owner, huge, (0, 0), (118, last), false);
    select_range(&owner, sibling, (0, last), (2, last), false);
    let sibling_frame = publication.acquire_view(sibling).unwrap();
    let sibling_text = owner.view_selection_text(sibling).unwrap();
    assert_eq!(
        owner
            .selection_text_view_bounded(huge, 1024 * 1024)
            .unwrap(),
        BoundedSelectionText::WorkLimitExceeded
    );
    assert_eq!(
        publication.view_generation(sibling),
        Some(sibling_frame.generation)
    );
    assert_eq!(
        owner
            .selection_text_view_bounded(sibling, sibling_text.len())
            .unwrap(),
        BoundedSelectionText::Text(sibling_text)
    );
    assert_eq!(sibling_frame.row_text(0), "é".repeat(119));
    // Legacy copy deliberately remains unbounded and proves the rejected
    // range really contains several MiB, rather than testing only coordinates.
    assert!(owner.view_selection_text(huge).unwrap().len() > 3 * 1024 * 1024);
    // This smaller range is below the cell-work cap but above 1 MiB in UTF-8.
    // It exercises the fixed-buffer overflow path, not the range preflight.
    select_range(&owner, huge, (0, 0), (118, 4_999), false);
    assert_eq!(
        owner
            .selection_text_view_bounded(huge, 1024 * 1024)
            .unwrap(),
        BoundedSelectionText::ByteLimitExceeded
    );
    assert_eq!(
        publication.view_generation(sibling),
        Some(sibling_frame.generation)
    );
}
