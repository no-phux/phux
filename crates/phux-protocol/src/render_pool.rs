//! Pooled libghostty render scaffolding, shared by both ends of the wire
//! (ADR-0013, ADR-0086).
//!
//! Every grid walker pools a [`RenderState`] + [`RowIterator`] +
//! [`CellIterator`] for the life of its pane. The hazard this type closes: a
//! pooled `RenderState` caches what it last walked, and libghostty's per-row
//! dirty bits are drained by whichever state reads a row first. After a
//! resize, or after the walked `Terminal` is replaced at the same geometry (a
//! client replica republish, whose recycled allocation can pass libghostty's
//! pointer-compared viewport pin as "unchanged"), a stale pooled state serves
//! old rows as `Clean`. [`RenderPool::begin`] rebuilds the trio when the
//! geometry or the caller's required [`TerminalGeneration`] changes.
//!
//! The pool owns allocation and geometry only. Dirty-bit clearing is a
//! per-consumer policy (the server's sync, incremental synthesis, and
//! reference diff, and the client renderer all differ), and the `Terminal`
//! is passed per walk because several pools can walk one terminal.

use libghostty_vt::{
    RenderState, Terminal as GhosttyTerminal,
    render::{CellIterator, RowIterator, Snapshot},
};

/// Opaque caller-chosen identity of the walked `Terminal`.
///
/// A change rebuilds the pool. 128 bits fit the client's stream + bootstrap ids unhashed; a
/// walker whose terminal is never replaced passes a constant.
pub type TerminalGeneration = u128;

/// One pooled walk of a terminal's grid; the members are disjoint borrows:
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
    /// The snapshot from this walk's `RenderState::update` (dirty bits
    /// already drained; clearing them is the caller's decision).
    pub snapshot: Snapshot<'alloc, 's>,
    /// The pool's row iterator, borrowed for the duration of the walk.
    pub rows: &'s mut RowIterator<'alloc>,
    /// The pool's cell iterator, borrowed for the duration of the walk.
    pub cells: &'s mut CellIterator<'alloc>,
}

/// A pooled [`RenderState`] + [`RowIterator`] + [`CellIterator`], rebuilt when
/// the walked terminal changes geometry or identity. One per walker.
#[derive(Debug)]
pub struct RenderPool<'alloc> {
    state: RenderState<'alloc>,
    rows: RowIterator<'alloc>,
    cells: CellIterator<'alloc>,
    /// The `(cols, rows)` last walked; `None` forces the first rebuild.
    last_dims: Option<(u16, u16)>,
    /// The generation of the last walk.
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

    /// Start a walk of `terminal`, first rebuilding the trio if its geometry
    /// or `generation` (which changes exactly when the walked `Terminal` is
    /// replaced) changed. Drains the terminal's dirty bits into the pooled
    /// state; what happens to them next is the caller's policy.
    pub fn begin<'s, 'cb>(
        &'s mut self,
        terminal: &GhosttyTerminal<'alloc, 'cb>,
        generation: TerminalGeneration,
    ) -> Result<RenderWalk<'alloc, 's>, libghostty_vt::Error> {
        self.rebuild_if_stale(terminal, generation)?;
        // Disjoint borrows: the snapshot borrows `state` only.
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

    /// Reallocate the trio when the geometry or generation changed.
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
    use libghostty_vt::{Terminal, render::Dirty};

    use super::*;

    fn terminal(cols: u16, rows: u16) -> Terminal<'static, 'static> {
        {
            let mut terminal = Terminal::new(cols, rows).expect("Terminal::new");
            terminal
                .set_scrollback_max_lines(Some(100))
                .expect("Terminal::new");
            terminal
        }
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

    /// A generation change rebuilds at identical geometry, so the new
    /// generation's first walk is `Full`, not the old cache's `Clean`. One
    /// terminal under a new token is what a replaced terminal on a recycled
    /// allocation looks like from the pool's seat.
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
