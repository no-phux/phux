//! Document selection over the runtime's owner-thread anchors: a range from
//! two handles (a search match, a gesture result), a pointer gesture, and
//! the selected text. The engine owns every operation; this layer only
//! lowers handles unchanged. Errors reuse [`SearchError`]: a stale anchor is
//! the same failure whether it came from Find or from a gesture.

use phux_client_runtime::engine::{
    EngineError, EngineHandle, SelectionGestureEvent, SelectionGestureResult,
};

use super::search::SearchError;
use super::*;

/// One pointer gesture step, in the engine's own vocabulary: `phase` is 0
/// press, 1 drag, 2 release; `clicks` is 1, 2 or 3 on a press; `handle` is
/// the previous step's for a drag or release. `column`/`row` are viewport
/// cells; the surface metrics let the engine extend a drag past the edge.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq)]
pub struct ProjectionSelectionGesture {
    pub phase: u32,
    pub clicks: u32,
    pub handle: u64,
    pub column: u16,
    pub row: u32,
    pub rectangle: bool,
    pub x: f64,
    pub y: f64,
    pub columns: u32,
    pub cell_width: u32,
    pub screen_height: u32,
    pub padding_left: u32,
}

impl From<ProjectionSelectionGesture> for SelectionGestureEvent {
    fn from(gesture: ProjectionSelectionGesture) -> Self {
        Self {
            phase: gesture.phase,
            clicks: gesture.clicks,
            handle: gesture.handle,
            column: gesture.column,
            rectangle: gesture.rectangle,
            row: gesture.row,
            x: gesture.x,
            y: gesture.y,
            columns: gesture.columns,
            cell_width: gesture.cell_width,
            screen_height: gesture.screen_height,
            padding_left: gesture.padding_left,
        }
    }
}

/// The range a gesture step left: `handle` continues the gesture; `start`
/// and `end` are anchor handles, zero when nothing is selected.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionSelectionRange {
    pub handle: u64,
    pub start: u64,
    pub end: u64,
}

impl From<SelectionGestureResult> for ProjectionSelectionRange {
    fn from(result: SelectionGestureResult) -> Self {
        Self {
            handle: result.handle,
            start: result.start,
            end: result.end,
        }
    }
}

#[uniffi::export]
impl RemoteClient {
    /// Select the document range between two anchor handles (a search
    /// match's `start`/`end`, or a gesture's range).
    pub fn set_projection_selection(
        &self,
        terminal_id: String,
        start: u64,
        end: u64,
        rectangle: bool,
    ) -> Result<(), SearchError> {
        self.with_engine(&terminal_id, |engine, id| {
            engine.set_selection(id, start, end, rectangle)
        })
    }

    pub fn clear_projection_selection(&self, terminal_id: String) -> Result<(), SearchError> {
        self.with_engine(&terminal_id, EngineHandle::clear_selection)
    }

    /// The selected text, empty when nothing is selected. Lossy on invalid
    /// UTF-8, which a terminal grid cannot hold anyway.
    pub fn projection_selection_text(&self, terminal_id: String) -> Result<String, SearchError> {
        self.with_engine(&terminal_id, EngineHandle::selection_text)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Drive one step of a pointer selection gesture.
    pub fn projection_selection_gesture(
        &self,
        terminal_id: String,
        gesture: ProjectionSelectionGesture,
    ) -> Result<ProjectionSelectionRange, SearchError> {
        self.with_engine(&terminal_id, |engine, id| {
            engine.selection_gesture(id, gesture.into())
        })
        .map(ProjectionSelectionRange::from)
    }
}

impl RemoteClient {
    fn with_engine<T>(
        &self,
        terminal_id: &str,
        call: impl FnOnce(&EngineHandle, &ResourceId) -> Result<T, EngineError>,
    ) -> Result<T, SearchError> {
        self.with_terminal(terminal_id, |client, id| {
            client
                .engine()
                .ok_or(EngineError::Stopped)
                .and_then(|engine| call(&engine, id))
        })
        .ok_or(SearchError::Unavailable)?
        .map_err(SearchError::from)
    }
}
