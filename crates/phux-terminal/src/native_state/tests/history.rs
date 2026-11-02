use super::*;

/// An engine failure at the frontier reaches every owner with its cause.
#[test]
fn a_failed_frontier_gives_every_owner_the_engine_reason() {
    let limits = BootstrapLimits::default();
    let mut manager =
        NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("native manager");
    let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
    manager
        .retain_generation(&cursor)
        .expect("second generation owner");
    history_at(&mut manager, &cursor, 0, max_bytes, max_rows)
        .expect("HISTORY_BEGIN from a live lease");
    for row in 0..200_000 {
        manager.vt_write(format!("evicting-{row:06}\r\n").as_bytes());
    }

    let first = history_at(&mut manager, &cursor, 1, max_bytes, max_rows)
        .expect_err("a pruned lease cannot serve a page");
    assert!(
        is_lease_tombstone(first),
        "output past the oldest leased page is a typed invalidation, got {first:?}"
    );
    let second = history_at(&mut manager, &cursor, 1, max_bytes, max_rows)
        .expect_err("the spent frontier cannot serve the second owner either");
    assert_eq!(
        second, first,
        "every owner must get the engine's reason, not a spent-handle status"
    );
    assert!(
        manager.generations[&cursor].capture.is_none(),
        "a cut the engine marked dead must not be kept"
    );
    assert_eq!(manager.generations[&cursor].failure, Some(first));
}

/// A wide, styled pane still pages under the default 1 MiB page limit.
#[test]
fn a_wide_styled_pane_pages_under_the_default_page_limit() {
    let limits = BootstrapLimits::default();
    let mut manager =
        NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("native manager");
    let (cursor, _, max_rows) = install_leased_generation(&mut manager, limits);
    let rows = drain_generation(
        &mut manager,
        &cursor,
        0,
        limits.max_history_page_bytes(),
        max_rows,
    )
    .expect("every slice fits the default page limit");
    assert!(rows > 500, "the pane's history was delivered: {rows} rows");
}

/// A live lease stays well-behaved across every way the terminal can
/// invalidate its history, and across the manager's own teardown.
#[test]
fn a_live_lease_survives_invalidation_and_manager_teardown() {
    let limits = BootstrapLimits::default();
    let mut manager =
        NativeTerminalManager::new(paged_history_terminal(), 2).expect("native manager");

    let alternate = lease_through(
        &mut manager,
        limits,
        b"\x1b[?1049halternate screen output\r\n\x1b[?1049l",
    );
    assert!(
        matches!(alternate, Ok(rows) if rows > 0),
        "the primary screen's history is untouched by an alternate-screen \
             visit: {alternate:?}"
    );
    let erased = lease_through(&mut manager, limits, b"\x1b[3J");
    assert!(
        matches!(
            erased,
            Err(NativeStateError::Stale | NativeStateError::Pruned)
        ),
        "erasing scrollback invalidates the cut: {erased:?}"
    );
    assert_eq!(
        lease_through(&mut manager, limits, b"\x1bc"),
        Err(NativeStateError::Reset),
        "a full reset invalidates the cut"
    );

    let (resized, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
    history_at(&mut manager, &resized, 0, max_bytes, max_rows)
        .expect("HISTORY_BEGIN from a live lease");
    manager.resize(21, 4, 8, 16).expect("resize live terminal");
    assert!(
        !manager.has_generation(&resized),
        "resize retires every generation; the actor tombstones them as Resize"
    );
    assert_eq!(
        manager.history_record_at(&resized, 1, max_bytes, max_rows),
        Err(NativeStateError::InvalidHandle)
    );

    let (live, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
    history_at(&mut manager, &live, 0, max_bytes, max_rows)
        .expect("HISTORY_BEGIN from a live lease");
    drop(manager);
}

#[test]
fn pixel_only_resize_preserves_a_live_history_cursor() {
    let limits = BootstrapLimits::default();
    let terminal = paged_history_terminal();
    let (cols, rows) = (terminal.cols().unwrap(), terminal.rows().unwrap());
    let mut manager = NativeTerminalManager::new(terminal, 2).unwrap();
    let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
    history_at(&mut manager, &cursor, 0, max_bytes, max_rows).unwrap();
    let before = history_at(&mut manager, &cursor, 1, max_bytes, max_rows).unwrap();
    manager.resize(cols, rows, 11, 19).unwrap();
    let after = history_at(&mut manager, &cursor, 1, max_bytes, max_rows).unwrap();
    assert_eq!(after.bytes, before.bytes);
}

/// Scroll-region output between READY and the first request leaves the
/// cut deliverable.
#[test]
fn scroll_region_output_after_ready_is_reported_not_misread() {
    fn lease_unrequested_through(mutation: &[u8]) -> Result<usize, NativeStateError> {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("terminal with headroom");
        terminal
            .set_scrollback_max_lines(Some(100_000))
            .expect("row headroom");
        terminal
            .set_scrollback_max_bytes(Some(16 * 1024 * 1024))
            .expect("byte headroom, so nothing prunes");
        terminal
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .expect("continuation");
        for row in 0..2_000 {
            terminal.vt_write(format!("history-{row:04}\r\n").as_bytes());
        }
        let limits = BootstrapLimits::default();
        let mut manager = NativeTerminalManager::new(terminal, 2).expect("native manager");
        let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
        manager.vt_write(mutation);
        drain_generation(&mut manager, &cursor, 0, max_bytes, max_rows)
    }

    let mut plain = Vec::new();
    let mut region = b"\x1b[1;23r\x1b[23;1H".to_vec();
    for row in 0..50 {
        plain.extend_from_slice(format!("plain-{row:02}\r\n").as_bytes());
        region.extend_from_slice(format!("region-{row:02}\r\n").as_bytes());
    }
    region.extend_from_slice(b"\x1b[24;1Hstatus line\x1b[r");

    let control = lease_unrequested_through(&plain);
    assert!(
        matches!(control, Ok(rows) if rows >= 1_500),
        "plain output after READY leaves the cut deliverable: {control:?}"
    );
    let region = lease_unrequested_through(&region);
    assert!(
        matches!(region, Ok(rows) if rows >= 1_500),
        "scroll-region output after READY must leave the cut deliverable, \
             not a different or truncated stream: {region:?}"
    );
}
