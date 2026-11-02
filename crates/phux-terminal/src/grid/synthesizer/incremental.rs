use super::{
    ConsumerReference, Dirty, GhosttyTerminal, ReferenceCursorMode, RenderWalk, SnapshotBytes,
    SnapshotSynthesizer, SynthesisError, TerminalGeneration, assemble_diff, emit_epilogue,
    emit_screen_mode, fresh_render_trio, grid_dims, render_row_body, walk_viewport_rows,
};

impl<'alloc> SnapshotSynthesizer<'alloc> {
    /// Synthesize one consumer's incremental diff against its own
    /// [`ConsumerReference`] (phux-ia4).
    ///
    /// libghostty's `RenderState::update` consumes the shared terminal's dirty
    /// bits, so with N consumers on one pane only the first would see changes.
    /// This compares rendered row bodies against the per-consumer reference
    /// instead, so each consumer gets a correct diff regardless of the others.
    ///
    /// A non-empty diff advances `reference` before returning (emit-once). An
    /// empty body means the viewport is byte-identical to the reference.
    pub fn synthesize_against_reference(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        reference: &mut ConsumerReference,
    ) -> Result<SnapshotBytes, SynthesisError> {
        // Single-consumer wrapper; `tick_emit` calls `prepare_tick` +
        // `diff_consumer` directly so N consumers share one render.
        let (cols, rows_n, live_cm) = self.prepare_tick(terminal)?;
        Ok(self.diff_consumer(cols, rows_n, live_cm, reference))
    }

    /// Serve several consumers from one render, the way the state-sync tick
    /// does: one `prepare_tick`, then one `diff_consumer`
    /// per reference, in order. Each non-empty diff advances its reference.
    ///
    /// # Errors
    ///
    /// As [`Self::synthesize_against_reference`].
    pub fn synthesize_tick(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        references: &mut [ConsumerReference],
    ) -> Result<Vec<SnapshotBytes>, SynthesisError> {
        let (cols, rows_n, live_cm) = self.prepare_tick(terminal)?;
        Ok(references
            .iter_mut()
            .map(|reference| self.diff_consumer(cols, rows_n, live_cm, reference))
            .collect())
    }

    /// The generation every pooled walk passes to [`RenderPool::begin`],
    /// bumped first (forcing a rebuild) if a foreign walk drained the
    /// terminal's dirty bits since the last pooled walk. Every pooled reader
    /// goes through here so none serves stale rows; only
    /// [`Self::prepare_tick`] clears the dirty flags a walk accumulates.
    pub(super) const fn pool_walk_generation(&mut self) -> TerminalGeneration {
        if self.foreign_walk.replace(false) {
            self.pool_generation = self.pool_generation.wrapping_add(1);
        }
        self.pool_generation
    }

    /// Render the grid once per tick into the shared `tick_*` buffers and
    /// return `(cols, rows, live_cm)`. Each consumer's
    /// [`Self::diff_consumer`] then diffs against them, so N consumers cost
    /// one render and one set of cursor/mode FFI reads.
    ///
    /// Only rows the pooled render state rebuilt since the last tick are
    /// re-rendered; the rest keep last tick's bodies, which are byte-identical
    /// because a clean row's cached cells are unchanged. A full render runs
    /// instead whenever that cannot be trusted: the first tick, a pool rebuild
    /// (geometry change, or a foreign walk drained the dirty bits), a
    /// libghostty full redraw (screen switch, viewport move, palette or other
    /// terminal-wide change), or a previous tick that failed midway.
    pub(crate) fn prepare_tick(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<(u16, u16, ReferenceCursorMode), SynthesisError> {
        let span = tracing::debug_span!(
            "prepare_tick",
            full = tracing::field::Empty,
            rendered_rows = tracing::field::Empty,
        )
        .entered();
        let generation = self.pool_walk_generation();
        let RenderWalk {
            snapshot,
            rows,
            cells,
        } = self.pool.begin(terminal, generation)?;
        let (cols, rows_n) = grid_dims(&snapshot)?;
        let dirty = snapshot.dirty()?;
        let rows_usize = usize::from(rows_n);
        let full = !self.tick_rows_valid
            || dirty == Dirty::Full
            || self.tick_cols != cols
            || self.tick_rows.len() != rows_usize;
        span.record("full", full);

        // Invalid until every dirty row is rendered and its flag cleared: an
        // error below leaves a partial state the next tick must not trust.
        self.tick_rows_valid = false;
        if full {
            // Clear every in-range buffer so a row the iterator skips cannot
            // leave stale content from a prior tick.
            self.tick_rows.resize_with(rows_usize, Vec::new);
            for body in &mut self.tick_rows {
                body.clear();
            }
        }
        let mut rendered_rows: usize = 0;
        if full || dirty != Dirty::Clean {
            // Fresh pen per row keeps each row body self-contained and
            // comparable across ticks.
            let tick_rows = &mut self.tick_rows;
            walk_viewport_rows(rows, &snapshot, rows_n, |row_index, row| {
                if !full && !row.dirty()? {
                    return Ok(());
                }
                let body = &mut tick_rows[usize::from(row_index)];
                body.clear();
                render_row_body(cells, row, body)?;
                row.set_dirty(false)?;
                rendered_rows += 1;
                Ok(())
            })?;
        }
        span.record("rendered_rows", rendered_rows);
        #[cfg(test)]
        {
            self.last_rendered_rows = rendered_rows;
        }
        // The global flag is independent of the row flags cleared above.
        snapshot.set_dirty(Dirty::Clean)?;
        self.tick_cols = cols;
        self.tick_rows_valid = true;

        // Cursor/mode + epilogue + screen toggle: consumer-independent, so
        // capture/precompute them once while the snapshot is live.
        let live_cm = ReferenceCursorMode::capture(&snapshot, terminal)?;
        self.tick_epilogue.clear();
        emit_epilogue(&mut self.tick_epilogue, &snapshot, terminal)?;
        self.tick_screen_toggle.clear();
        emit_screen_mode(&mut self.tick_screen_toggle, terminal)?;
        Ok((cols, rows_n, live_cm))
    }

    /// Record that `terminal` was walked through a render state other than
    /// the pool. That walk drained the terminal's dirty bits, so the pool's
    /// cached rows may be stale; the next [`Self::prepare_tick`] rebuilds the
    /// pool and renders in full.
    pub(crate) fn note_foreign_walk(&self) {
        self.foreign_walk.set(true);
    }

    /// Diff one consumer against the shared tick buffers from
    /// [`Self::prepare_tick`], advancing its reference (emit-once). An empty
    /// body means the consumer is already current.
    pub(crate) fn diff_consumer(
        &self,
        cols: u16,
        rows_n: u16,
        live_cm: ReferenceCursorMode,
        reference: &mut ConsumerReference,
    ) -> SnapshotBytes {
        let span = tracing::debug_span!(
            "diff_consumer",
            changed_row_count = tracing::field::Empty,
            out_bytes = tracing::field::Empty,
        )
        .entered();
        // A dimension change clears the reference so every row repaints.
        if reference.cols != cols || reference.rows != rows_n {
            reference.reset_geometry(cols, rows_n);
        }

        // Clone changed rows into the reference: `tick_rows` is shared with
        // the other consumers this tick.
        {
            let ConsumerReference {
                rows_body,
                changed_scratch: changed,
                ..
            } = &mut *reference;
            changed.clear();
            for (idx, (rendered, stored)) in
                self.tick_rows.iter().zip(rows_body.iter_mut()).enumerate()
            {
                if *stored != *rendered {
                    stored.clear();
                    stored.extend_from_slice(rendered);
                    changed.push(u16::try_from(idx).unwrap_or(u16::MAX));
                }
            }
        }

        let cursor_mode_changed = reference.cursor_mode != live_cm;
        if reference.changed_scratch.is_empty() && !cursor_mode_changed {
            span.record("changed_row_count", 0_usize);
            span.record("out_bytes", 0_usize);
            return SnapshotBytes {
                cols,
                rows: rows_n,
                bytes: Vec::new(),
                scrollback: Vec::new(),
            };
        }
        let changed_row_count = reference.changed_scratch.len();

        // Screen toggle first, and only on this consumer's alt-screen
        // transition, so content lands on the right buffer.
        let toggle: &[u8] = if reference.cursor_mode.alt_screen_set() == live_cm.alt_screen_set() {
            &[]
        } else {
            &self.tick_screen_toggle
        };
        let out = assemble_diff(
            toggle,
            &reference.changed_scratch,
            &reference.rows_body,
            &self.tick_epilogue,
        );
        reference.cursor_mode = live_cm;

        span.record("changed_row_count", changed_row_count);
        span.record("out_bytes", out.len());
        SnapshotBytes {
            cols,
            rows: rows_n,
            bytes: out,
            scrollback: Vec::new(),
        }
    }

    /// Diff the current tick against `base` without advancing it (ADR-0042).
    /// `base` is the consumer's last-acked reference, so a dropped frame
    /// self-heals: its rows still differ from `base` next tick. Each changed
    /// row is repainted in full, making the delta idempotent. Requires a
    /// preceding [`Self::prepare_tick`]; the caller advances `base` via
    /// [`Self::snapshot_tick_reference`] when a `FRAME_ACK` lands.
    pub(crate) fn diff_against_base(
        &self,
        cols: u16,
        rows_n: u16,
        live_cm: ReferenceCursorMode,
        base: &ConsumerReference,
    ) -> Vec<u8> {
        let span = tracing::debug_span!(
            "diff_against_base",
            changed_row_count = tracing::field::Empty,
            out_bytes = tracing::field::Empty,
        )
        .entered();
        let geometry_mismatch = base.cols != cols || base.rows != rows_n;
        let screen_changed = base.cursor_mode.alt_screen_set() != live_cm.alt_screen_set();
        let cursor_mode_changed = base.cursor_mode != live_cm;

        // Rows that differ from the acked reference, or all rows on a
        // geometry mismatch.
        let mut changed: Vec<u16> = Vec::new();
        for (idx, rendered) in self.tick_rows.iter().enumerate() {
            let differs = geometry_mismatch
                || base
                    .rows_body
                    .get(idx)
                    .is_none_or(|stored| stored != rendered);
            if differs {
                changed.push(u16::try_from(idx).unwrap_or(u16::MAX));
            }
        }

        if changed.is_empty() && !cursor_mode_changed {
            span.record("changed_row_count", 0_usize);
            span.record("out_bytes", 0_usize);
            return Vec::new();
        }

        let toggle: &[u8] = if screen_changed {
            &self.tick_screen_toggle
        } else {
            &[]
        };
        let out = assemble_diff(toggle, &changed, &self.tick_rows, &self.tick_epilogue);
        span.record("changed_row_count", changed.len());
        span.record("out_bytes", out.len());
        out
    }

    /// Snapshot the current tick's rendered grid as a standalone
    /// [`ConsumerReference`], the diff base once the matching ack lands
    /// (ADR-0042). Requires a preceding [`Self::prepare_tick`].
    pub(crate) fn snapshot_tick_reference(
        &self,
        cols: u16,
        rows_n: u16,
        live_cm: ReferenceCursorMode,
    ) -> ConsumerReference {
        let mut reference = ConsumerReference::new();
        reference.cols = cols;
        reference.rows = rows_n;
        reference.rows_body.clone_from(&self.tick_rows);
        reference.cursor_mode = live_cm;
        reference
    }

    /// Prime `reference` to the current `terminal` state without emitting
    /// bytes, so the first diff after attach reports only later changes.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "`&mut self` is retained for semver compatibility of this externally visible method"
    )]
    pub fn prime_reference(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        reference: &mut ConsumerReference,
    ) -> Result<(), SynthesisError> {
        // The attach snapshot just walked with a fresh render state and
        // consumed the terminal's dirty bits, so the pool may hold older
        // rows. Prime from another fresh walk to match the snapshot's cut;
        // the next tick rebuilds the pool for the same reason.
        self.note_foreign_walk();
        let (mut render_state, mut rows, mut cells) = fresh_render_trio()?;
        let snapshot = render_state.update(terminal)?;
        let (cols, rows_n) = grid_dims(&snapshot)?;
        reference.reset_geometry(cols, rows_n);

        walk_viewport_rows(&mut rows, &snapshot, rows_n, |row_index, row| {
            let mut body: Vec<u8> = Vec::with_capacity(usize::from(cols));
            render_row_body(&mut cells, row, &mut body)?;
            reference.rows_body[usize::from(row_index)] = body;
            Ok(())
        })?;
        reference.cursor_mode = ReferenceCursorMode::capture(&snapshot, terminal)?;
        Ok(())
    }
}
