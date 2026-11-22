use super::*;
use crate::ViewId;
use phux_client_core::grid::CELL_SELECTED;

mod document;
mod lifecycle;
mod presentation;
mod sync_output;

fn output(owner: &EngineHandle, terminal: &ResourceId, seq: u64, bytes: &[u8]) {
    apply_ok(owner, super::output(terminal, seq, bytes));
}

fn point(column: u16, row: u32) -> EngineDocumentPoint {
    EngineDocumentPoint {
        space: 1,
        column,
        row,
    }
}

fn select(owner: &EngineHandle, view: ViewId, row: u32, end: u16) -> (u64, u64) {
    let start = owner.track_view_anchor(view, point(0, row)).unwrap();
    let end = owner.track_view_anchor(view, point(end, row)).unwrap();
    owner.set_view_selection(view, start, end, false).unwrap();
    (start, end)
}

fn held_text(frame: &GridFrame) -> String {
    frame.text()
}

fn gesture(phase: u32, handle: u64, column: u16) -> SelectionGestureEvent {
    SelectionGestureEvent {
        phase,
        clicks: 1,
        handle,
        column,
        rectangle: false,
        row: 0,
        x: f64::from(column) * 10.0,
        y: 0.0,
        columns: 20,
        cell_width: 10,
        screen_height: 40,
        padding_left: 0,
    }
}
