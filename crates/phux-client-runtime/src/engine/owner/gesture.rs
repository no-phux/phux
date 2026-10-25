//! Pointer selection gestures and whole-document text operations.

use super::{
    EngineDocumentPoint, EngineError, FormatOptions, Gesture, Owner, Point, PointCoordinate,
    PointerGesture, ResourceId, Selection, SelectionGestureEvent, SelectionGestureResult,
    TextRegion, apply_gesture_event, engine_error, gesture_document_points, validate_gesture,
};

impl Owner {
    #[cfg(feature = "engine")]
    pub(super) fn selection_gesture(
        &mut self,
        id: &ResourceId,
        event: SelectionGestureEvent,
    ) -> Result<SelectionGestureResult, EngineError> {
        validate_gesture(event)?;
        let token = self
            .replica_identity(id)
            .map(|identity| identity.0)
            .ok_or_else(|| engine_error("terminal has no published READY generation"))?;
        let mut state = self.gesture_state(id, event, token)?;
        let result = self.gesture_snapshot(id, event, &mut state);
        self.finish_gesture(id, event, state, result.is_err());
        result
    }

    #[cfg(feature = "engine")]
    fn gesture_state(
        &mut self,
        id: &ResourceId,
        event: SelectionGestureEvent,
        token: u128,
    ) -> Result<PointerGesture, EngineError> {
        if event.phase == 0 {
            self.clear_selection(id)?;
            let handle = crate::view::allocate_handle()
                .ok_or_else(|| engine_error("gesture handles exhausted"))?;
            return Ok(PointerGesture {
                token,
                handle,
                gesture: Gesture::new().map_err(|error| engine_error(error.to_string()))?,
            });
        }
        let valid = self
            .presentations
            .get(id)
            .and_then(|state| state.gesture.as_ref())
            .is_some_and(|state| state.token == token && state.handle == event.handle);
        if !valid {
            return Err(engine_error("stale selection gesture"));
        }
        self.presentation_mut(id)?
            .gesture
            .take()
            .ok_or_else(|| engine_error("missing selection gesture"))
    }

    #[cfg(feature = "engine")]
    fn finish_gesture(
        &mut self,
        id: &ResourceId,
        event: SelectionGestureEvent,
        mut state: PointerGesture,
        failed: bool,
    ) {
        if event.phase == 2 || failed {
            if let Ok(terminal) = self.terminal(id) {
                state.gesture.reset(terminal);
            }
        } else if let Some(presentation) = self.presentations.get_mut(id) {
            presentation.gesture = Some(state);
        }
    }

    #[cfg(feature = "engine")]
    fn gesture_snapshot(
        &mut self,
        id: &ResourceId,
        event: SelectionGestureEvent,
        state: &mut PointerGesture,
    ) -> Result<SelectionGestureResult, EngineError> {
        let mut result = SelectionGestureResult {
            handle: state.handle,
            ..SelectionGestureResult::default()
        };
        if event.phase == 2 {
            return Ok(result);
        }
        let terminal = self.terminal(id)?;
        let grid_ref = terminal
            .grid_ref(Point::Viewport(PointCoordinate {
                x: event.column,
                y: event.row,
            }))
            .map_err(|error| engine_error(error.to_string()))?;
        let selected = apply_gesture_event(terminal, grid_ref, event, &mut state.gesture)?;
        let Some(selected) = selected else {
            self.clear_selection(id)?;
            return Ok(result);
        };
        let (start, end) = gesture_document_points(terminal, &selected)?;
        (result.start, result.end) =
            self.commit_gesture_selection(id, start, end, event.rectangle)?;
        Ok(result)
    }

    #[cfg(feature = "engine")]
    fn commit_gesture_selection(
        &mut self,
        id: &ResourceId,
        start: EngineDocumentPoint,
        end: EngineDocumentPoint,
        rectangle: bool,
    ) -> Result<(u64, u64), EngineError> {
        let start = self.track_anchor(id, start)?;
        let end = match self.track_anchor(id, end) {
            Ok(anchor) => anchor,
            Err(error) => {
                let _ = self.release_anchor(id, start);
                return Err(error);
            }
        };
        if let Err(error) = self.set_selection(id, start, end, rectangle) {
            let _ = self.release_anchor(id, start);
            let _ = self.release_anchor(id, end);
            return Err(error);
        }
        let old = std::mem::replace(
            &mut self.presentation_mut(id)?.gesture_anchors,
            vec![start, end],
        );
        for handle in old {
            self.drop_anchor_handle(id, handle);
        }
        Ok((start, end))
    }

    /// Ghostty's own select-all span, committed like a gesture selection so
    /// its anchors follow the content and are released with the next one.
    #[cfg(feature = "engine")]
    pub(super) fn select_all(&mut self, id: &ResourceId) -> Result<bool, EngineError> {
        self.reset_gesture(id);
        let points = {
            let terminal = self.terminal(id)?;
            let selected = terminal
                .select_all()
                .map_err(|error| engine_error(error.to_string()))?;
            selected
                .map(|selected| gesture_document_points(terminal, &selected))
                .transpose()?
        };
        let Some((start, end)) = points else {
            self.clear_selection(id)?;
            return Ok(false);
        };
        self.commit_gesture_selection(id, start, end, false)?;
        Ok(true)
    }

    #[cfg(feature = "engine")]
    pub(super) fn region_text(
        &self,
        id: &ResourceId,
        region: TextRegion,
        max_bytes: usize,
    ) -> Result<super::BoundedSelectionText, EngineError> {
        let terminal = self.terminal(id)?;
        let error = |error: libghostty_vt::Error| engine_error(error.to_string());
        let cols = terminal.cols().map_err(error)?;
        let rows = match region {
            TextRegion::Screen => terminal.total_rows(),
            TextRegion::History => terminal.scrollback_rows(),
        }
        .map_err(error)?;
        let Some(last) = rows.checked_sub(1) else {
            return Ok(super::BoundedSelectionText::Text(Vec::new()));
        };
        let last = u32::try_from(last).map_err(|_| engine_error("region exceeds u32 rows"))?;
        let corner = |x, y| terminal.grid_ref(Point::History(PointCoordinate { x, y }));
        let selection = Selection::new(
            corner(0, 0).map_err(error)?,
            corner(cols.saturating_sub(1), last).map_err(error)?,
            false,
        );
        // The span is the loaded document itself, already bounded by the
        // history budget, so the formatter's work is too; only the output
        // size is refused here.
        let formatted = terminal
            .format_selection_alloc(
                None,
                FormatOptions::new()
                    .with_selection(&selection)
                    .with_unwrap(true)
                    .with_trim(true),
            )
            .map_err(error)?;
        Ok(match formatted {
            Some(bytes) if bytes.len() > max_bytes => {
                super::BoundedSelectionText::ByteLimitExceeded
            }
            Some(bytes) => super::BoundedSelectionText::Text(bytes.to_vec()),
            None => super::BoundedSelectionText::Text(Vec::new()),
        })
    }
}
