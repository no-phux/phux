//! One terminal-scoped render cache, with independently owned frame buffers.

use super::{
    Arc, EngineError, GridDamage, GridFrame, Owner, ProjectorSlot, ResourceId, Scrollbar,
    apply_default_colors, engine_error, frame_colors, monotonic_ms, terminal_defaults,
};

impl Owner {
    /// The sole dirty reader survives every view switch, including failure.
    pub(super) fn render_and_publish(&mut self, id: &ResourceId) -> Result<bool, EngineError> {
        let Some((token, geometry, stream, bootstrap, sequence)) = self.replica_identity(id) else {
            return Ok(false);
        };
        self.ensure_projector(id, token, geometry)?;
        let views = self.views.values().any(|view| &view.terminal == id);
        self.install_presentation(id)?;
        let Some(mut slot) = self.projectors.remove(id) else {
            return Ok(false);
        };
        // Paced publication can leave one presentation many projections
        // behind another; its row damage then says nothing about its own
        // previous frame.
        let full = views || slot.reader != self.active_view;
        slot.reader = self.active_view;
        let _timed = crate::perf::PROJECT.timer();
        let result = self.project_frame(id, &mut slot, full, (stream, bootstrap, sequence));
        self.projectors.insert(id.clone(), slot);
        let result = result.and_then(|frame| self.publish_frame(id, frame));
        if result.is_err() {
            crate::perf::PROJECT_ERRORS.incr();
        }
        result?;
        crate::perf::PUBLISHED.incr();
        Ok(true)
    }

    fn project_frame(
        &mut self,
        id: &ResourceId,
        slot: &mut ProjectorSlot,
        full: bool,
        generation: (u64, u64, u64),
    ) -> Result<GridFrame, EngineError> {
        let _timed = crate::perf::PROJECT_GRID.timer();
        let terminal = self.terminal(id)?;
        let defaults = terminal_defaults(terminal)?;
        let alternate = matches!(
            terminal.active_screen(),
            Ok(libghostty_vt::screen::Screen::Alternate)
        );
        let snapshot = slot
            .projector
            .project(terminal, slot.token)
            .map_err(|error| engine_error(error.to_string()))?;
        let mut colors = frame_colors(&snapshot.colors);
        apply_default_colors(&mut colors, defaults);
        let bar = terminal
            .scrollbar()
            .map_err(|error| engine_error(format!("scrollbar: {error}")))?;
        let mut frame = GridFrame {
            terminal_id: id.clone(),
            generation: 0,
            stream_id: generation.0,
            bootstrap_id: generation.1,
            last_seq: generation.2,
            cols: snapshot.cols,
            rows: snapshot.rows,
            cursor: snapshot.cursor,
            scrollbar: Scrollbar {
                total: bar.total,
                offset: bar.offset,
                len: bar.len,
            },
            colors,
            // Publication has no metadata-only damage channel. Multi-view
            // damage is conservative because the cache's last reader may be
            // another view; default-only row damage remains exact.
            damage: match (full, snapshot.damage) {
                (true, _) | (_, GridDamage::Clean) => GridDamage::Full,
                (_, damage) => damage,
            },
            buffer: self.presentation_mut(id)?.spare.take().unwrap_or_default(),
        };
        slot.projector.swap_buffer(&mut frame.buffer);
        if full {
            frame.buffer.row_dirty.fill(true);
        }
        self.presentation_mut(id)?.predictor.apply(
            frame.cols,
            frame.rows,
            alternate,
            &mut frame.cursor,
            &mut frame.buffer,
            monotonic_ms(),
        );
        Ok(frame)
    }

    fn publish_frame(&mut self, id: &ResourceId, frame: GridFrame) -> Result<(), EngineError> {
        let _timed = crate::perf::PUBLISH_SWAP.timer();
        let previous = match self.active_view {
            Some(view) => self.publication.publish_view(view, frame),
            None => self.publication.publish(id, frame),
        };
        if let Some(previous) = previous {
            match Arc::try_unwrap(previous) {
                Ok(previous) => self.presentation_mut(id)?.spare = Some(previous.buffer),
                Err(_) => crate::perf::BUFFER_HELD.incr(),
            }
        }
        Ok(())
    }
}
