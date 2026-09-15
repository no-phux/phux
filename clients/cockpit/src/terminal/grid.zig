//! Compatibility facade for terminal session, projection, and painting seams.

const session = @import("session.zig");
const render = @import("render.zig");

pub const max_cols = session.max_cols;
pub const max_rows = session.max_rows;
pub const max_cells = session.max_cells;
pub const snapshot_text_capacity = session.snapshot_text_capacity;
pub const Session = session.Session;
pub const CellBox = session.CellBox;

pub const PaintOptions = render.PaintOptions;
pub const RowFit = render.RowFit;
pub const paneIdBase = render.paneIdBase;
pub const cursorCommandId = render.cursorCommandId;
pub const idNamespace = render.idNamespace;
pub const idNamespaceOf = render.idNamespaceOf;
pub const id_namespace_stride = render.id_namespace_stride;
pub const cursor_command_id = render.cursor_command_id;
pub const paint = render.paint;
pub const paintTerminalGrid = render.paintTerminalGrid;
pub const cropLastN = render.cropLastN;
pub const cropToFit = render.cropToFit;

test {
    _ = render;
}
