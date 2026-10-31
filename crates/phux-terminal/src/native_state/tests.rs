use super::*;
use std::io::Cursor;

use libghostty_vt::snapshot::Decoder;
use phux_protocol::caps::BootstrapProfileKind;

fn snapshot_ready_offset(bytes: &[u8]) -> Result<usize, NativeStateError> {
    let mut reader = Cursor::new(bytes);
    let decoder = Decoder::new(&mut reader)?;
    drop(decoder.ready()?);
    let offset = usize::try_from(reader.position()).unwrap_or(bytes.len());
    Ok(offset.min(bytes.len()))
}

fn terminal(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    let mut terminal = GhosttyTerminal::new(cols, rows).expect("canonical terminal");
    terminal
        .set_scrollback_max_lines(Some(1000))
        .expect("scrollback");
    terminal
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .expect("continuation");
    terminal
}

fn history_terminal() -> GhosttyTerminal<'static, 'static> {
    let mut terminal = terminal(20, 4);
    for row in 0..300 {
        terminal.vt_write(format!("history-{row:03}\r\n").as_bytes());
    }
    terminal
}

/// Wide enough that retained scrollback is a HISTORY suffix, not only
/// the READY active-area pages a 20x4 grid can fold into the prefix.
fn paged_history_terminal() -> GhosttyTerminal<'static, 'static> {
    let mut terminal = GhosttyTerminal::new(80, 24).expect("paged history terminal");
    terminal
        .set_scrollback_max_lines(Some(100_000))
        .expect("history rows");
    terminal
        .set_scrollback_max_bytes(None)
        .expect("unlimited scrollback bytes");
    terminal
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .expect("continuation");
    for row in 0..2_000 {
        terminal.vt_write(format!("history-{row:04}\r\n").as_bytes());
    }
    terminal
}

fn drain_ready(capture: &mut NativeManagedCapture<'_>) {
    loop {
        let required = match capture.step(&mut []) {
            Err(NativeStateError::OutOfSpace { required_bytes, .. }) => required_bytes,
            other => panic!("probe: {other:?}"),
        };
        let mut exact = vec![0; required];
        if matches!(
            capture.step(&mut exact).expect("record").kind,
            NativeCheckpointChunkKind::Ready
        ) {
            break;
        }
    }
}

fn history_at(
    manager: &mut NativeTerminalManager,
    cursor: &OpaqueHistoryCursor,
    index: usize,
    max_bytes: u32,
    max_rows: u32,
) -> Result<CachedNativeHistoryRecord, NativeStateError> {
    loop {
        match manager.history_record_at(cursor, index, max_bytes, max_rows) {
            Ok(record) => return Ok(record),
            Err(NativeStateError::ImportBusy) => {}
            Err(error) => return Err(error),
        }
    }
}

fn next_record(
    manager: &mut NativeTerminalManager,
    cursor: &OpaqueHistoryCursor,
    index: usize,
    limits: BootstrapLimits,
) -> CachedNativeHistoryRecord {
    history_at(
        manager,
        cursor,
        index,
        limits.max_history_page_bytes(),
        phux_protocol::MAX_HISTORY_PAGE_ROWS,
    )
    .unwrap_or_else(|error| panic!("history record: {error:?}"))
}

/// A 200x50 terminal of styled full rows, pruned to `history_bytes`.
fn deep_terminal(history_bytes: usize) -> GhosttyTerminal<'static, 'static> {
    let mut terminal = GhosttyTerminal::new(200, 50).expect("deep terminal");
    terminal
        .set_scrollback_max_lines(Some(10_000_000))
        .expect("history rows");
    terminal
        .set_scrollback_max_bytes(Some(history_bytes))
        .expect("retained byte ceiling");
    terminal
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .expect("continuation");
    let body = "abcdefghij klmnopqrst uvwxyz0123 456789ABCD EFGHIJKLMN OPQRSTUVWX                     YZabcdefgh ijklmnopqr stuvwxyz01 23456789AB CDEFGHIJKL MNOPQRSTUV                     WXYZabcdef ghijklmnop qrstuvwx";
    // Overshoot the ceiling so pruning, not the write volume, decides how
    // much history the capture actually has to account for.
    for row in 0..(history_bytes / 64 + 4_000) {
        terminal.vt_write(
            format!("\x1b[38;5;{}m{row:06} \x1b[0m{body}\r\n", 16 + row % 216).as_bytes(),
        );
    }
    terminal
}

/// Best-of-`samples` wall time one attach spends in the mutation
/// exclusion.
fn best_detach_cost(
    manager: &mut NativeTerminalManager,
    limits: BootstrapLimits,
    samples: usize,
) -> std::time::Duration {
    let mut best = std::time::Duration::MAX;
    for _ in 0..samples {
        let mut capture = manager
            .begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
            .expect("generation capture");
        drain_ready(&mut capture);
        let started = std::time::Instant::now();
        let detached = manager.finish_generation_capture(capture);
        best = best.min(started.elapsed());
        drop(detached.expect("detach generation READY continuation"));
    }
    best
}

fn detach_managed_generation(
    manager: &mut NativeTerminalManager,
    limits: BootstrapLimits,
) -> (OpaqueHistoryCursor, NativeGenerationSeed) {
    let mut capture = manager
        .begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
        .expect("generation capture");
    drain_ready(&mut capture);
    manager
        .finish_generation_capture(capture)
        .expect("detach generation READY continuation")
}

fn install_leased_generation(
    manager: &mut NativeTerminalManager,
    limits: BootstrapLimits,
) -> (OpaqueHistoryCursor, u32, u32) {
    let (cursor, seed) = detach_managed_generation(manager, limits);
    let bounds = seed.bounds();
    let reserved = bounds
        .required_reserved_bytes()
        .expect("bounded generation reservation");
    manager
        .install_generation(cursor, seed, bounds, reserved)
        .expect("install leased generation");
    (
        cursor,
        u32::try_from(bounds.max_record_bytes).expect("protocol byte bound"),
        u32::try_from(bounds.max_rows).expect("protocol row bound"),
    )
}

fn drain_generation(
    manager: &mut NativeTerminalManager,
    cursor: &OpaqueHistoryCursor,
    start: usize,
    max_bytes: u32,
    max_rows: u32,
) -> Result<usize, NativeStateError> {
    let mut rows = 0;
    let mut index = start;
    loop {
        let record = history_at(manager, cursor, index, max_bytes, max_rows)?;
        rows += record.rows;
        if record.finish {
            return Ok(rows);
        }
        index += 1;
    }
}

fn lease_through(
    manager: &mut NativeTerminalManager,
    limits: BootstrapLimits,
    mutation: &[u8],
) -> Result<usize, NativeStateError> {
    for row in 0..2_000 {
        manager.vt_write(format!("history-{row:04}\r\n").as_bytes());
    }
    let (cursor, max_bytes, max_rows) = install_leased_generation(manager, limits);
    let first = history_at(manager, &cursor, 0, max_bytes, max_rows)
        .expect("HISTORY_BEGIN from a live lease");
    assert!(
        !first.finish,
        "fixture must have a history page before FINISH"
    );
    manager.vt_write(mutation);
    let outcome = drain_generation(manager, &cursor, 1, max_bytes, max_rows);
    manager
        .release_generation(&cursor)
        .expect("release leased generation");
    outcome
}

const fn is_lease_tombstone(error: NativeStateError) -> bool {
    matches!(
        error,
        NativeStateError::Pruned
            | NativeStateError::Stale
            | NativeStateError::Reset
            | NativeStateError::Resize
            | NativeStateError::WrongGeneration
    )
}

mod bootstrap;
mod browser_fixture;
mod history;
