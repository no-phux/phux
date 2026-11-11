//! Nestable DEC 2026 synchronized-output blocks.

use std::io::{self, Write};

/// DEC 2026 "begin synchronized update" — the outer terminal buffers
/// everything until the matching end and presents it in one composite.
pub(in crate::attach) const SYNC_OUTPUT_BEGIN: &[u8] = b"\x1b[?2026h";
/// DEC 2026 "end synchronized update".
pub(in crate::attach) const SYNC_OUTPUT_END: &[u8] = b"\x1b[?2026l";

thread_local! {
    /// How many [`SyncOutput`] guards are open on this thread. DEC 2026 is a
    /// mode, not a counter, so only the outermost guard may emit the mode
    /// bytes. Thread-local because the two nesting layers never see each
    /// other's state and the paint path runs on one thread.
    static SYNC_OUTPUT_DEPTH: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

/// An open DEC 2026 synchronized-output block, nestable: only the outermost
/// `begin`/`end` emit `CSI ? 2026 h`/`l`. The depth is released in `Drop`, so
/// an early return cannot strand it (the driver's sync-output watchdog backs
/// up a transaction left open). Closing is explicit because the guard cannot
/// hold the sink across the paint's borrow of `out`.
#[derive(Debug)]
#[must_use = "an opened synchronized-output block must be closed with `end`"]
pub(in crate::attach) struct SyncOutput {
    /// Whether THIS guard emitted the begin bytes (i.e. it is the outermost).
    outermost: bool,
}

impl SyncOutput {
    /// Open a synchronized-output block around the emits that follow.
    pub(in crate::attach) fn begin(out: &mut impl Write) -> io::Result<Self> {
        let outermost = SYNC_OUTPUT_DEPTH.with(|depth| {
            let was = depth.get();
            depth.set(was.saturating_add(1));
            was == 0
        });
        // Bind the guard BEFORE the write so a failing sink drops it — and
        // releases the depth it just took — instead of leaking a level that
        // would silently suppress every later block on this thread.
        let guard = Self { outermost };
        if outermost {
            out.write_all(SYNC_OUTPUT_BEGIN)?;
        }
        Ok(guard)
    }

    /// Close the block, emitting the end bytes only if this guard opened it.
    pub(in crate::attach) fn end(self, out: &mut impl Write) -> io::Result<()> {
        if self.outermost {
            out.write_all(SYNC_OUTPUT_END)?;
        }
        Ok(())
    }
}

impl Drop for SyncOutput {
    fn drop(&mut self) {
        SYNC_OUTPUT_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}
