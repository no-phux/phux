//! One terminal-scoped render cache, with independently owned frame buffers.

use super::{
    Arc, EngineError, GridBuffer, GridDamage, GridFrame, Owner, ProjectorSlot, ResourceId,
    Scrollbar, apply_default_colors, engine_error, frame_colors, monotonic_ms, terminal_defaults,
};

/// Frames a presentation published and has since replaced, kept until every
/// consumer lets go so a later projection refills their buffers instead of
/// allocating. A painter holds the frame it painted until it paints the next
/// one, which is after the publication that replaces it, so the buffer
/// usually comes back one projection later.
#[derive(Default)]
pub(super) struct RetiredFrames(Vec<Arc<GridFrame>>);

impl RetiredFrames {
    /// Frames kept at once. The oldest goes when a newer one arrives, and
    /// its buffer is freed by whichever consumer drops it last.
    const LIMIT: usize = 2;

    fn retire(&mut self, frame: Arc<GridFrame>) {
        if self.0.len() == Self::LIMIT {
            self.0.remove(0);
        }
        self.0.push(frame);
    }

    /// The buffer of a retired frame no consumer holds any more. The
    /// publication table keeps no weak references, so a count of one is this
    /// list's own.
    fn reuse(&mut self) -> Option<GridBuffer> {
        let index = self
            .0
            .iter()
            .position(|frame| Arc::strong_count(frame) == 1)?;
        Arc::try_unwrap(self.0.remove(index))
            .ok()
            .map(|frame| frame.buffer)
    }
}

impl Owner {
    /// The sole dirty reader survives every view switch, including failure.
    pub(super) fn render_and_publish(&mut self, id: &ResourceId) -> Result<bool, EngineError> {
        let Some((token, geometry, stream, bootstrap, sequence)) = self.replica_identity(id) else {
            return Ok(false);
        };
        self.ensure_projector(id, token, geometry)?;
        self.install_presentation(id)?;
        let Some(mut slot) = self.projectors.remove(id) else {
            return Ok(false);
        };
        // Damage belongs to the projector's last reader. A switch between the
        // default presentation and an independent view needs a full frame;
        // skipped publications within one view are checked by the painter's
        // generation guard.
        let full = !slot.valid || slot.reader != self.active_view;
        slot.reader = self.active_view;
        let _timed = crate::perf::PROJECT.timer();
        let projected = self.project_frame(id, &mut slot, full, (stream, bootstrap, sequence));
        slot.valid = projected.is_ok();
        self.projectors.insert(id.clone(), slot);
        let result = projected.and_then(|frame| self.publish_frame(id, frame));
        if result.is_err() {
            if let Some(slot) = self.projectors.get_mut(id) {
                slot.valid = false;
            }
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
        let bar = terminal
            .scrollbar()
            .map_err(|error| engine_error(format!("scrollbar: {error}")))?;
        // A scrolled-back viewport also carries the row below it, so a
        // consumer can scroll by less than a row without another turn.
        let below = bar.offset.saturating_add(bar.len);
        let fringe = (below < bar.total)
            .then(|| u32::try_from(below).ok())
            .flatten();
        let snapshot = slot
            .projector
            .project_with_fringe(terminal, slot.token, fringe)
            .map_err(|error| engine_error(error.to_string()))?;
        let mut colors = frame_colors(&snapshot.colors);
        apply_default_colors(&mut colors, defaults);
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
            images: Vec::new(),
            // Publication has no metadata-only damage channel. Multi-view
            // damage is conservative because the cache's last reader may be
            // another view; default-only row damage remains exact.
            damage: match (full, snapshot.damage) {
                (true, _) | (_, GridDamage::Clean) => GridDamage::Full,
                (_, damage) => damage,
            },
            buffer: self
                .presentation_mut(id)?
                .retired
                .reuse()
                .unwrap_or_else(|| {
                    crate::perf::BUFFER_FRESH.incr();
                    GridBuffer::default()
                }),
        };
        slot.projector.swap_buffer(&mut frame.buffer);
        frame.images = slot
            .images
            .project(self.terminal(id)?, &frame.buffer, frame.cols, frame.rows)
            .map_err(|error| engine_error(format!("image projection: {error}")))?;
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
            self.presentation_mut(id)?.retired.retire(previous);
        }
        Ok(())
    }
}
