//! Pooled libghostty render scaffolding, shared by both ends of the wire.
//!
//! Under [ADR-0013] a libghostty `Terminal` runs on the server *and* on the
//! client, and each end walks its grid through the same three libghostty
//! objects: a [`RenderState`], a [`RowIterator`], and a [`CellIterator`].
//! Allocating that trio is not free, so every walker pools it for the life of
//! the pane it serves.
//!
//! Pooling has one non-obvious hazard, and it is the reason this type exists
//! rather than four private copies of the same three fields:
//!
//! > A pooled [`RenderState`] caches what it last walked. libghostty's per-row
//! > dirty bits live on the `Terminal` and are drained by whichever
//! > `RenderState` reads a row first, so after a geometry change a pooled state
//! > can report the *new* dimensions while still serving *pre-resize* row
//! > bodies (`phux-5pyx`). A freshly allocated state has no prior cache, so its
//! > first walk observes every row as it is now.
//!
//! [`RenderPool::begin`] therefore rebuilds the trio whenever the terminal's
//! `(cols, rows)` differ from the last walk, and hands back the three objects
//! as disjoint borrows so a caller can drive the row/cell walk exactly as it
//! did with three private fields.
//!
//! Geometry is not the only way a pooled state can go stale. The client
//! REPLACES a pane's `Terminal` wholesale when a replica generation is
//! republished (bootstrap resync), and a swap at identical geometry leaves
//! `last_dims` equal while the pooled state's cache — including the viewport
//! pin libghostty consults to decide whether a walk may skip clean rows —
//! still belongs to the previous `Terminal`. That pin is compared by copied
//! pointer value, so a replacement terminal whose pages land at a recycled
//! address can masquerade as "unchanged" and be served the old terminal's
//! rows as `Clean`. So [`RenderPool::begin`] also takes a
//! [`TerminalGeneration`]: the caller names the identity of the terminal it
//! walks, and a change of that token rebuilds the trio even at identical
//! geometry (`phux-994s`).
//!
//! That token is a **required** argument rather than a second entry point.
//! A walker whose terminal is fixed for the pool's life (the server walks one
//! PTY-backed `Terminal` per pane for the pane's whole life) passes a
//! constant, which is exactly "a generation that never changes"; a walker
//! whose terminal can be replaced passes the live token. One entry point means
//! there is no wrong one to reach for — and the failure mode of reaching for
//! the wrong one was silent, allocator-dependent grid corruption.
//!
//! # What this type deliberately does NOT own
//!
//! **Dirty policy.** `RenderState::update` *consumes* the terminal's dirty
//! bits; when and whether to clear [`Snapshot::set_dirty`] and each row's
//! `set_dirty` is a per-consumer decision, and phux's consumers legitimately
//! disagree: the server's `mark_synced` clears both, its
//! `synthesize_incremental` clears neither (an unacked diff must stay
//! re-emittable, ADR-0018), its per-consumer reference diff bypasses the dirty
//! bits entirely, and the client's renderer clears only the rows it drew.
//! Folding those into one type would erase four deliberate policies, so the
//! pool owns allocation and geometry only and leaves every dirty decision at
//! the call site.
//!
//! **The `Terminal`.** The terminal is passed to [`RenderPool::begin`] per
//! walk rather than owned here. On the server one `Terminal` is walked by
//! several pools (one per consumer); on the client the pool outlives
//! individual replica generations. Owning it would be wrong at both ends.
//!
//! This module carries no wire types and does not participate in protocol
//! versioning. It lives in `phux-protocol` behind the `server` feature for the
//! same reason [`crate::sgr`] and [`crate::kitty_replay`] do: it is a
//! libghostty-backed render helper that both `phux-server` and `phux-client`
//! need, and `phux-core` (the only other crate both could import) deliberately
//! carries no `libghostty-vt` dependency. See [ADR-0086].
//!
//! [ADR-0013]: https://github.com/phall1/phux/blob/main/ADR/0013-libghostty-bytes-on-wire.md
//! [ADR-0018]: https://github.com/phall1/phux/blob/main/ADR/0018-lazy-state-synchronization.md
//! [ADR-0086]: https://github.com/phall1/phux/blob/main/ADR/0086-shared-render-pool.md

use libghostty_vt::{
    RenderState, Terminal as GhosttyTerminal,
    render::{CellIterator, RowIterator, Snapshot},
};

/// Opaque caller-chosen identity for the `Terminal` a pool walks.
///
/// The pool never inspects the value; it only compares it against the token
/// of the previous walk, and a change rebuilds the pooled trio even at
/// identical geometry (see [`RenderPool::begin`]). 128 bits so a composite
/// identity — the client packs its replica key's non-zero 64-bit stream and
/// bootstrap ids — fits without hashing or collision. A walker whose terminal
/// is never replaced passes a constant.
pub type TerminalGeneration = u128;

/// One pooled walk of a terminal's grid.
///
/// Returned by [`RenderPool::begin`]. The three members are disjoint borrows
/// of the pool, so the usual walk still type-checks:
///
/// ```ignore
/// let RenderWalk { snapshot, rows, cells } = pool.begin(terminal, generation)?;
/// let mut row_iter = rows.update(&snapshot)?;
/// while let Some(row) = row_iter.next() {
///     let mut cell_iter = cells.update(row)?;
///     // ...
/// }
/// ```
#[derive(Debug)]
pub struct RenderWalk<'alloc, 's> {
    /// The snapshot produced by this walk's `RenderState::update`.
    ///
    /// Reading [`Snapshot::dirty`] drains nothing further; the drain already
    /// happened inside `update`. Clearing it is the caller's decision.
    pub snapshot: Snapshot<'alloc, 's>,
    /// The pool's row iterator, borrowed for the duration of the walk.
    pub rows: &'s mut RowIterator<'alloc>,
    /// The pool's cell iterator, borrowed for the duration of the walk.
    pub cells: &'s mut CellIterator<'alloc>,
}

/// A pooled [`RenderState`] + [`RowIterator`] + [`CellIterator`], rebuilt when
/// the terminal it walks changes geometry or identity.
///
/// Allocate one per walker (per pane, per consumer) and keep it warm across
/// frames; see the module docs for the pooling hazard it exists to close.
#[derive(Debug)]
pub struct RenderPool<'alloc> {
    state: RenderState<'alloc>,
    rows: RowIterator<'alloc>,
    cells: CellIterator<'alloc>,
    /// The `(cols, rows)` this pool last walked, or `None` before the first
    /// walk. A change rebuilds the trio.
    last_dims: Option<(u16, u16)>,
    /// The caller-supplied terminal identity of the last [`Self::begin`]
    /// walk. A change rebuilds the trio even at identical geometry. The
    /// pre-first-walk value is arbitrary — `last_dims` is `None` until the
    /// first walk, so that walk rebuilds regardless of the token it carries.
    last_generation: TerminalGeneration,
}

impl<'alloc> RenderPool<'alloc> {
    /// Allocate a fresh pool. Do this once per walker, not once per frame.
    pub fn new() -> Result<Self, libghostty_vt::Error> {
        Ok(Self {
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            last_dims: None,
            last_generation: 0,
        })
    }

    /// The `(cols, rows)` this pool last walked, or `None` before the first
    /// [`Self::begin`].
    #[must_use]
    pub const fn last_dims(&self) -> Option<(u16, u16)> {
        self.last_dims
    }

    /// Start a walk of `terminal` on behalf of the caller-named identity
    /// `generation`, rebuilding the pooled trio first if either that token
    /// or the terminal's geometry changed since the last walk.
    ///
    /// The token names *which terminal* this pool is walking, not a frame or
    /// content revision: pass a value that changes exactly when the walked
    /// `Terminal` object is replaced (the client passes its replica
    /// generation), and the pool discards the previous terminal's cache
    /// instead of letting it masquerade as this one's (`phux-994s`). A walker
    /// whose terminal is fixed for the pool's life passes a constant.
    ///
    /// This performs the `RenderState::update` that **drains `terminal`'s
    /// dirty bits into the pooled state**; what the caller then does with
    /// [`Snapshot::dirty`] and the per-row bits is entirely the caller's
    /// policy (see the module docs).
    pub fn begin<'s, 'cb>(
        &'s mut self,
        terminal: &GhosttyTerminal<'alloc, 'cb>,
        generation: TerminalGeneration,
    ) -> Result<RenderWalk<'alloc, 's>, libghostty_vt::Error> {
        self.rebuild_if_stale(terminal, generation)?;
        // Destructure so the snapshot (which borrows `state`) and the two
        // iterators are disjoint borrows rather than three overlapping
        // borrows of `self`.
        let Self {
            state, rows, cells, ..
        } = self;
        let snapshot = state.update(terminal)?;
        Ok(RenderWalk {
            snapshot,
            rows,
            cells,
        })
    }

    /// Discard and reallocate the pooled trio when `terminal`'s dimensions
    /// differ from the last walk (`phux-5pyx`), or when the caller-supplied
    /// identity token says the walked `Terminal` was replaced since the last
    /// walk (`phux-994s`).
    ///
    /// Scoped to the rare resize/republish tick rather than every call, so
    /// the pooled allocation win survives on the steady-state hot path.
    fn rebuild_if_stale<'cb>(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, 'cb>,
        generation: TerminalGeneration,
    ) -> Result<(), libghostty_vt::Error> {
        let live = (terminal.cols()?, terminal.rows()?);
        if generation != self.last_generation || self.last_dims != Some(live) {
            self.state = RenderState::new()?;
            self.rows = RowIterator::new()?;
            self.cells = CellIterator::new()?;
            self.last_dims = Some(live);
        }
        self.last_generation = generation;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use libghostty_vt::{Terminal, TerminalOptions, render::Dirty};

    use super::*;

    fn terminal(cols: u16, rows: u16) -> Terminal<'static, 'static> {
        Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 100,
        })
        .expect("Terminal::new")
    }

    /// One pooled walk under the "clear everything drawn" dirty policy:
    /// return the walk's dirty classification, then reset both layers the
    /// way a renderer that painted every reported row would.
    fn walk_and_clear(
        pool: &mut RenderPool<'static>,
        terminal: &Terminal<'static, 'static>,
        generation: TerminalGeneration,
    ) -> Dirty {
        let RenderWalk { snapshot, rows, .. } = pool.begin(terminal, generation).expect("begin");
        let dirty = snapshot.dirty().expect("dirty");
        let mut row_iter = rows.update(&snapshot).expect("rows.update");
        while let Some(row) = row_iter.next() {
            row.set_dirty(false).expect("row.set_dirty");
        }
        snapshot
            .set_dirty(Dirty::Clean)
            .expect("snapshot.set_dirty");
        dirty
    }

    /// phux-994s: a generation change rebuilds the pooled state even at
    /// identical geometry, so the first walk of the new generation reports
    /// `Dirty::Full` instead of serving the previous generation's
    /// already-painted cache as `Clean`.
    ///
    /// Driven against ONE terminal on purpose. The pool cannot observe the
    /// walked terminal's allocation, so the same terminal under a new token
    /// is exactly what a REPLACED terminal whose pages recycled the old
    /// allocation looks like from the pool's seat — the case libghostty's
    /// own viewport-pin comparison cannot catch and the caller-supplied
    /// token exists to.
    #[test]
    fn generation_change_rebuilds_at_identical_geometry() {
        let mut t = terminal(10, 2);
        t.vt_write(b"AA");
        let mut pool = RenderPool::new().expect("pool");

        assert_eq!(
            walk_and_clear(&mut pool, &t, 1),
            Dirty::Full,
            "a fresh pool's first walk observes every row"
        );
        assert_eq!(
            walk_and_clear(&mut pool, &t, 1),
            Dirty::Clean,
            "same generation, same geometry, no writes: nothing to draw"
        );
        assert_eq!(pool.last_dims(), Some((10, 2)));
        assert_eq!(pool.last_generation, 1);

        let dirty = walk_and_clear(&mut pool, &t, 2);
        assert_eq!(pool.last_generation, 2);
        assert_eq!(
            dirty,
            Dirty::Full,
            "a new generation at unchanged geometry must rebuild the pooled \
             state, not serve the previous generation's Clean cache"
        );
    }
}
