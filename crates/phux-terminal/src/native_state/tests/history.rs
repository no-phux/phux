use super::*;

#[test]
fn image_payload_is_a_zero_row_history_unit_not_finish() {
    let mut terminal = GhosttyTerminal::new(20, 4).unwrap();
    terminal
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal).unwrap();
    terminal.resize(20, 4, 8, 16).unwrap();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=2,r=2,C=1;/wAA/w==\x1b\\");
    let mut manager = NativeTerminalManager::new(terminal, 2).unwrap();
    let (cursor, max_bytes, max_rows) =
        install_leased_generation(&mut manager, BootstrapLimits::default());
    let image = history_at(&manager, &cursor, 0, max_bytes, max_rows).unwrap();
    assert_eq!(image.rows, 0);
    assert!(
        !image.finish,
        "zero image rows must not terminate history delivery"
    );
    assert!(!image.bytes.is_empty());
    let finish = history_at(&manager, &cursor, 1, max_bytes, max_rows).unwrap();
    assert!(finish.finish);
    assert_eq!(finish.rows, 0);
}

/// The full retained-storage cap is checked before READY, not after handing
/// a client a history cursor which cannot finish. The source stays usable.
#[test]
fn over_cap_image_snapshot_refuses_capture_without_poisoning_the_terminal() {
    let mut terminal = GhosttyTerminal::new(20, 4).unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal).unwrap();
    // Fill almost the entire 64 MiB image budget using 48 KiB records. Their
    // metadata/envelopes make the encoded snapshot larger than 64 MiB even
    // though every individual unit fits the negotiated 1 MiB page limit.
    let pixels = "A".repeat(64 * 1024);
    let count = 64 * 1024 * 1024 / (128 * 128 * 3);
    for id in 1..=count {
        terminal
            .vt_write(format!("\x1b_Ga=T,f=24,s=128,v=128,i={id},q=2;{pixels}\x1b\\").as_bytes());
    }
    let mut manager = NativeTerminalManager::new(terminal, 1).unwrap();
    assert_eq!(
        manager.capture(BootstrapLimits::default()).unwrap_err(),
        NativeStateError::LimitExceeded
    );
    assert!(!manager.capture_active);
    assert!(
        manager
            .try_terminal()
            .unwrap()
            .kitty_graphics()
            .unwrap()
            .unwrap()
            .image(u32::try_from(count).unwrap())
            .is_some()
    );
    manager.vt_write(b"\x1b_Ga=d,d=A,q=2;\x1b\\still-live");
    let capture = manager
        .capture(BootstrapLimits::default())
        .expect("capture works after removing image data");
    manager.abort_generation_capture(capture);
}

#[test]
fn oversized_image_unit_is_refused_before_ready_without_losing_the_source() {
    let mut terminal = GhosttyTerminal::new(20, 4).unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal).unwrap();
    let pixels = "A".repeat(4 * 1024 * 1024);
    terminal.vt_write(format!("\x1b_Ga=T,f=24,s=1024,v=1024,i=42,q=2;{pixels}\x1b\\").as_bytes());
    let mut manager = NativeTerminalManager::new(terminal, 1).unwrap();
    assert!(
        manager
            .try_terminal()
            .unwrap()
            .kitty_graphics()
            .unwrap()
            .unwrap()
            .image(42)
            .is_some()
    );
    assert_eq!(
        manager.capture(BootstrapLimits::default()).unwrap_err(),
        NativeStateError::LimitExceeded
    );
    assert!(manager.try_terminal().is_some());
    assert!(!manager.capture_active);
    manager.vt_write(b"\x1b_Ga=d,d=A,q=2;\x1b\\live-after-refusal");
    let capture = manager.capture(BootstrapLimits::default()).unwrap();
    manager.abort_generation_capture(capture);
}

/// Encoded history is immutable, shared, and independent of source pruning.
#[test]
fn immutable_history_survives_source_pruning_for_every_owner() {
    let limits = BootstrapLimits::default();
    let mut manager =
        NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("native manager");
    let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
    manager
        .retain_generation(&cursor)
        .expect("second generation owner");
    let expected =
        history_at(&manager, &cursor, 1, max_bytes, max_rows).expect("encoded second page");
    for row in 0..200_000 {
        manager.vt_write(format!("evicting-{row:06}\r\n").as_bytes());
    }

    let first = history_at(&manager, &cursor, 1, max_bytes, max_rows)
        .expect("source pruning cannot change encoded history");
    let second = history_at(&manager, &cursor, 1, max_bytes, max_rows)
        .expect("the second owner reads the same immutable page");
    assert_eq!(first, expected);
    assert_eq!(second, expected);
    assert_eq!(first.bytes.as_ptr(), second.bytes.as_ptr());
    assert!(drain_generation(&manager, &cursor, 0, max_bytes, max_rows).unwrap() > 0);
}

/// A wide, styled pane still pages under the default 1 MiB page limit.
#[test]
fn a_wide_styled_pane_pages_under_the_default_page_limit() {
    let limits = BootstrapLimits::default();
    let mut manager =
        NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("native manager");
    let (cursor, _, max_rows) = install_leased_generation(&mut manager, limits);
    let rows = drain_generation(
        &manager,
        &cursor,
        0,
        limits.max_history_page_bytes(),
        max_rows,
    )
    .expect("every slice fits the default page limit");
    assert!(rows > 500, "the pane's history was delivered: {rows} rows");
}

/// Only generation lifecycle changes invalidate encoded history; source
/// clear/reset cannot mutate bytes that have already been captured.
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
        matches!(erased, Ok(rows) if rows > 0),
        "erasing source scrollback preserves encoded history: {erased:?}"
    );
    let reset = lease_through(&mut manager, limits, b"\x1bc");
    assert!(
        matches!(reset, Ok(rows) if rows > 0),
        "a source reset preserves encoded history: {reset:?}"
    );

    let (resized, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
    history_at(&manager, &resized, 0, max_bytes, max_rows)
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
    history_at(&manager, &live, 0, max_bytes, max_rows).expect("HISTORY_BEGIN from a live lease");
    drop(manager);
}

#[test]
fn pixel_only_resize_preserves_a_live_history_cursor() {
    let limits = BootstrapLimits::default();
    let terminal = paged_history_terminal();
    let (cols, rows) = (terminal.cols().unwrap(), terminal.rows().unwrap());
    let mut manager = NativeTerminalManager::new(terminal, 2).unwrap();
    let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
    history_at(&manager, &cursor, 0, max_bytes, max_rows).unwrap();
    let before = history_at(&manager, &cursor, 1, max_bytes, max_rows).unwrap();
    manager.resize(cols, rows, 11, 19).unwrap();
    let after = history_at(&manager, &cursor, 1, max_bytes, max_rows).unwrap();
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
        drain_generation(&manager, &cursor, 0, max_bytes, max_rows)
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
