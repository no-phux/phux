//! The composited frame: one DEC 2026 block shipped as one write.

use std::io::Write;

use crate::attach::render::SyncOutput;

/// Hide the cursor for the duration of a composited frame, so it never skates
/// across a half-painted grid on a terminal that ignores mode 2026.
const CURSOR_HIDE: &[u8] = b"\x1b[?25l";

/// One composited frame: a DEC 2026 block that swallows the flushes of
/// everything painted inside it and ships once, as one write and one flush.
///
/// A `Write` wrapper, so each painter keeps the flushes it needs standalone.
/// The block's depth is taken in [`Self::begin`], before any painter runs,
/// so the pane renderer's own [`SyncOutput`] nests instead of closing the
/// frame early. The prologue is buffered and shipped by [`Self::end`] only if
/// a painter wrote something, so an idle frame costs zero bytes and no
/// writer-thread wake. The buffer comes from a thread-local pool.
pub(in crate::attach) struct FrameBlock<'a, W: Write> {
    inner: &'a mut W,
    /// The frame's bytes, including the block prologue and epilogue.
    /// `mem::take`n from the thread-local pool at `begin` and returned at
    /// `end`.
    body: Vec<u8>,
    /// The nestable DEC 2026 guard. Held for the frame's whole life so the
    /// per-pane renderer's own guard nests inside it. `None` only if the
    /// scratch buffer somehow refused a write, which a `Vec` cannot.
    sync: Option<SyncOutput>,
    /// Whether any painter has emitted through the `Write` impl. The
    /// prologue and epilogue are written directly to `body`, so this counts
    /// painter output only.
    painted: bool,
}

thread_local! {
    /// Scratch buffer reused across composited frames (the paint path is one
    /// thread; a nested frame just takes a fresh buffer).
    static FRAME_BODY: std::cell::RefCell<Vec<u8>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl<'a, W: Write> FrameBlock<'a, W> {
    /// Begin a composited frame on `inner`: the block opens now, but nothing
    /// reaches `inner` until [`Self::end`]. The frame opens by deleting the
    /// kitty images closed panes' renderers left on the outer terminal.
    pub(in crate::attach) fn begin(inner: &'a mut W) -> Self {
        let mut body = FRAME_BODY.with(|pool| {
            pool.try_borrow_mut()
                .map(|mut pool| std::mem::take(&mut *pool))
                .unwrap_or_default()
        });
        body.clear();
        // Infallible: the sink is a `Vec`. `ok()` keeps the guard optional so
        // a future fallible sink degrades to "no block" rather than panicking.
        let sync = SyncOutput::begin(&mut body).ok();
        let _ = body.write_all(CURSOR_HIDE);
        let mut block = Self {
            inner,
            body,
            sync,
            painted: false,
        };
        // Infallible for the same reason.
        let _ = phux_protocol::kitty_replay::write_orphaned_deletes(&mut block);
        block
    }

    /// Whether any painter inside this frame has emitted a byte (an unchanged
    /// frame needs no cursor tail).
    pub(in crate::attach) const fn opened(&self) -> bool {
        self.painted
    }

    /// Close the block and ship it: `?2026l`, one write, one flush. A frame in
    /// which nothing was written closes to a no-op.
    pub(in crate::attach) fn end(mut self) -> std::io::Result<()> {
        if let Some(sync) = self.sync.take() {
            let _ = sync.end(&mut self.body);
        }
        let shipped = if self.painted {
            phux_client::perf::PAINTS.add(1);
            let _timed = phux_client::perf::PAINT_SUBMIT.timer();
            self.inner
                .write_all(&self.body)
                .and_then(|()| self.inner.flush())
        } else {
            Ok(())
        };
        // Return the buffer to the pool whatever the sink did.
        let body = std::mem::take(&mut self.body);
        FRAME_BODY.with(|pool| {
            if let Ok(mut pool) = pool.try_borrow_mut() {
                *pool = body;
            }
        });
        shipped
    }
}

impl<W: Write> Drop for FrameBlock<'_, W> {
    /// Release the sync-output depth if `end` was never reached.
    fn drop(&mut self) {
        self.sync = None;
    }
}

impl<W: Write> Write for FrameBlock<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.painted = true;
        self.body.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.painted = true;
        self.body.extend_from_slice(buf);
        Ok(())
    }

    /// Swallowed. The whole point: painters inside a composited frame must
    /// not each ship their partial work to the terminal.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
