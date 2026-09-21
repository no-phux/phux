//! Native terminal projection across `UniFFI`.
//!
//! The cell definition and flattening walk are `phux-client-core::grid`'s.
//! `UniFFI` copies one 36-byte POD array and one UTF-8 arena instead of lowering
//! a Rust `String` record for every viewport cell.

use std::sync::mpsc::{self, Receiver, Sender};

use libghostty_vt::{
    screen::Screen,
    terminal::{ScrollViewport, Terminal},
};
use phux_client_core::grid::{Cell, CursorStyle as CoreCursorStyle, GridDamage, GridProjector};

/// An RGB color already resolved through the terminal palette.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<libghostty_vt::style::RgbColor> for Color {
    fn from(color: libghostty_vt::style::RgbColor) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

/// The cursor's requested shape.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

/// One immutable projected viewport.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct GridProjection {
    pub cols: u16,
    pub rows: u16,
    /// Concatenated 36-byte `phux_client_core::grid::Cell` records.
    pub cells: Vec<u8>,
    /// Shared arena addressed by every cell's `utf8_offset`/`utf8_len`.
    pub utf8: Vec<u8>,
    pub cursor_col: Option<u16>,
    pub cursor_row: Option<u16>,
    pub cursor_visible: bool,
    pub cursor_shape: CursorShape,
    pub cursor_blinking: bool,
    pub default_fg: Color,
    pub default_bg: Color,
    /// Runtime publication generation; local engines advance on changed grids.
    pub generation: u64,
    /// Rows changed by this generation; all rows for the first/full frame.
    pub dirty_rows: Vec<u16>,
    pub scrollbar: ScrollbarState,
}

struct EngineInner {
    terminal: Terminal<'static, 'static>,
    projector: GridProjector,
    predictor: crate::predict::Predictor,
    generation: u64,
}

enum EngineCommand {
    Mutate(EngineMutation),
    Query(EngineQuery),
}

enum EngineMutation {
    Write(Vec<u8>),
    Resize(u16, u16, Sender<Result<(), EngineError>>),
    Predict(String),
    ClearPredictions,
    Scroll(i64),
    ScrollToBottom,
}

enum EngineQuery {
    IsAltScreen(Sender<bool>),
    Scrollbar(Sender<Result<ScrollbarState, EngineError>>),
    Render(Sender<Result<GridProjection, EngineError>>),
}

/// A live local terminal engine exposed as an opaque handle.
#[derive(uniffi::Object)]
pub struct TerminalEngine {
    commands: Sender<EngineCommand>,
}

#[uniffi::export]
impl TerminalEngine {
    #[uniffi::constructor]
    pub fn new(cols: u16, rows: u16, scrollback: u32) -> Result<std::sync::Arc<Self>, EngineError> {
        let (commands, receiver) = mpsc::channel();
        let (initialized, initialization) = mpsc::channel();
        std::thread::Builder::new()
            .name("phux-engine-owner".to_owned())
            .spawn(move || engine_owner(receiver, initialized, cols, rows, scrollback))
            .map_err(|error| EngineError::Engine(format!("owner thread: {error}")))?;
        initialization.recv().map_err(|_| {
            EngineError::Engine("owner thread stopped during initialization".to_owned())
        })??;
        Ok(std::sync::Arc::new(Self { commands }))
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self
            .commands
            .send(EngineCommand::Mutate(EngineMutation::Write(bytes)));
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), EngineError> {
        self.request(|reply| EngineCommand::Mutate(EngineMutation::Resize(cols, rows, reply)))?
    }

    pub fn predict_text(&self, text: String) {
        let _ = self
            .commands
            .send(EngineCommand::Mutate(EngineMutation::Predict(text)));
    }

    pub fn clear_predictions(&self) {
        let _ = self
            .commands
            .send(EngineCommand::Mutate(EngineMutation::ClearPredictions));
    }

    pub fn scroll_viewport(&self, delta: i64) {
        let _ = self
            .commands
            .send(EngineCommand::Mutate(EngineMutation::Scroll(delta)));
    }

    pub fn is_alt_screen(&self) -> bool {
        self.request(|reply| EngineCommand::Query(EngineQuery::IsAltScreen(reply)))
            .unwrap_or(false)
    }

    pub fn scroll_to_bottom(&self) {
        let _ = self
            .commands
            .send(EngineCommand::Mutate(EngineMutation::ScrollToBottom));
    }

    pub fn scrollbar(&self) -> Result<ScrollbarState, EngineError> {
        self.request(|reply| EngineCommand::Query(EngineQuery::Scrollbar(reply)))?
    }

    pub fn render_grid(&self) -> Result<GridProjection, EngineError> {
        self.request(|reply| EngineCommand::Query(EngineQuery::Render(reply)))?
    }
}

impl TerminalEngine {
    fn request<T>(
        &self,
        command: impl FnOnce(Sender<T>) -> EngineCommand,
    ) -> Result<T, EngineError> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(command(reply))
            .map_err(|_| EngineError::Engine("owner thread stopped".to_owned()))?;
        response
            .recv()
            .map_err(|_| EngineError::Engine("owner thread stopped".to_owned()))
    }
}

fn engine_owner(
    commands: Receiver<EngineCommand>,
    initialized: Sender<Result<(), EngineError>>,
    cols: u16,
    rows: u16,
    scrollback: u32,
) {
    let mut engine = match EngineInner::new(cols, rows, scrollback) {
        Ok(engine) => engine,
        Err(error) => {
            let _ = initialized.send(Err(error));
            return;
        }
    };
    if initialized.send(Ok(())).is_err() {
        return;
    }
    while let Ok(command) = commands.recv() {
        engine.apply(command);
    }
}

impl EngineInner {
    fn new(cols: u16, rows: u16, scrollback: u32) -> Result<Self, EngineError> {
        let mut terminal = Terminal::new(cols.max(1), rows.max(1))
            .map_err(|error| EngineError::Engine(format!("terminal_new: {error:?}")))?;
        terminal
            .set_scrollback_max_lines(Some(scrollback as usize))
            .map_err(|error| EngineError::Engine(format!("scrollback_max_lines: {error:?}")))?;
        terminal
            .set_scrollback_max_bytes((scrollback == 0).then_some(0))
            .map_err(|error| EngineError::Engine(format!("scrollback_max_bytes: {error:?}")))?;
        let mut predictor = crate::predict::Predictor::default();
        predictor.set_viewport(cols.max(1), rows.max(1));
        Ok(Self {
            terminal,
            projector: GridProjector::new()
                .map_err(|error| EngineError::Engine(error.to_string()))?,
            predictor,
            generation: 0,
        })
    }

    fn apply(&mut self, command: EngineCommand) {
        match command {
            EngineCommand::Mutate(command) => self.mutate(command),
            EngineCommand::Query(command) => self.query(command),
        }
    }

    fn mutate(&mut self, command: EngineMutation) {
        match command {
            EngineMutation::Write(bytes) => self.terminal.vt_write(&bytes),
            EngineMutation::Resize(cols, rows, reply) => {
                self.predictor.set_viewport(cols.max(1), rows.max(1));
                let result = self
                    .terminal
                    .resize(cols.max(1), rows.max(1), 0, 0)
                    .map_err(|error| EngineError::Engine(format!("resize: {error:?}")));
                let _ = reply.send(result);
            }
            EngineMutation::Predict(text) => self.predictor.predict(&self.terminal, &text),
            EngineMutation::ClearPredictions => self.predictor.clear(),
            EngineMutation::Scroll(delta) => self.terminal.scroll_viewport(ScrollViewport::Delta(
                isize::try_from(delta).unwrap_or_else(|_| {
                    if delta.is_negative() {
                        isize::MIN
                    } else {
                        isize::MAX
                    }
                }),
            )),
            EngineMutation::ScrollToBottom => {
                self.terminal.scroll_viewport(ScrollViewport::Bottom);
            }
        }
    }

    fn query(&mut self, command: EngineQuery) {
        match command {
            EngineQuery::IsAltScreen(reply) => {
                let _ = reply.send(matches!(
                    self.terminal.active_screen(),
                    Ok(Screen::Alternate)
                ));
            }
            EngineQuery::Scrollbar(reply) => {
                let result = scrollbar(&self.terminal);
                let _ = reply.send(result);
            }
            EngineQuery::Render(reply) => {
                let _ = reply.send(self.render());
            }
        }
    }

    fn render(&mut self) -> Result<GridProjection, EngineError> {
        let snapshot = self
            .projector
            .project(&self.terminal)
            .map_err(|error| EngineError::Engine(error.to_string()))?;
        let cols = snapshot.cols;
        let rows = snapshot.rows;
        let mut cursor = snapshot.cursor;
        let colors = snapshot.colors.clone();
        let damage = snapshot.damage;
        let mut cells = snapshot.buffer.cells.clone();
        let mut utf8 = snapshot.buffer.utf8.clone();
        let row_dirty = snapshot.buffer.row_dirty.clone();
        self.predictor
            .apply(&self.terminal, cols, &mut cursor, &mut cells, &mut utf8);
        if damage != GridDamage::Clean {
            self.generation = self.generation.wrapping_add(1).max(1);
        }
        Ok(GridProjection {
            cols,
            rows,
            cells: encode_cells(&cells),
            utf8,
            cursor_col: cursor.visible.then_some(cursor.col),
            cursor_row: cursor.visible.then_some(cursor.row),
            cursor_visible: cursor.visible,
            cursor_shape: cursor_shape(cursor.style),
            cursor_blinking: cursor.blinking,
            default_fg: colors.foreground.into(),
            default_bg: colors.background.into(),
            generation: self.generation,
            dirty_rows: row_dirty
                .iter()
                .enumerate()
                .filter(|(_, dirty)| **dirty)
                .filter_map(|(row, _)| u16::try_from(row).ok())
                .collect(),
            scrollbar: scrollbar(&self.terminal)?,
        })
    }
}

fn cursor_shape(style: CoreCursorStyle) -> CursorShape {
    match style {
        CoreCursorStyle::Block => CursorShape::Block,
        CoreCursorStyle::Underline => CursorShape::Underline,
        CoreCursorStyle::BlockHollow => CursorShape::BlockHollow,
        CoreCursorStyle::Bar => CursorShape::Bar,
    }
}

/// Serialize one `#[repr(C)]` cell without reading Rust padding. Bytes 18-19
/// and 35 are explicitly zero, matching the shared layout's pinned offsets.
pub(crate) fn encode_cells(cells: &[Cell]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(cells.len() * 36);
    for cell in cells {
        bytes.extend_from_slice(&cell.utf8_offset.to_ne_bytes());
        bytes.extend_from_slice(&cell.utf8_len.to_ne_bytes());
        bytes.extend_from_slice(&cell.content_tag.to_ne_bytes());
        bytes.extend_from_slice(&cell.hyperlink_offset.to_ne_bytes());
        bytes.extend_from_slice(&cell.hyperlink_len.to_ne_bytes());
        bytes.push(cell.wide);
        bytes.push(cell.semantic_content);
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&cell.flags.to_ne_bytes());
        bytes.extend_from_slice(&[
            cell.foreground_r,
            cell.foreground_g,
            cell.foreground_b,
            cell.background_r,
            cell.background_g,
            cell.background_b,
            cell.underline,
            cell.underline_r,
            cell.underline_g,
            cell.underline_b,
            cell.reserved,
            0,
        ]);
    }
    bytes
}

#[derive(uniffi::Error, Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine error: {0}")]
    Engine(String),
}

#[derive(uniffi::Record, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollbarState {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

fn scrollbar(terminal: &Terminal<'static, 'static>) -> Result<ScrollbarState, EngineError> {
    terminal
        .scrollbar()
        .map(|bar| ScrollbarState {
            total: bar.total,
            offset: bar.offset,
            len: bar.len,
        })
        .map_err(|error| EngineError::Engine(format!("scrollbar: {error:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(projection: &GridProjection, index: usize) -> &str {
        let start = index * 36;
        let offset = u32::from_ne_bytes(projection.cells[start..start + 4].try_into().unwrap());
        let len = u16::from_ne_bytes(projection.cells[start + 4..start + 6].try_into().unwrap());
        std::str::from_utf8(&projection.utf8[offset as usize..offset as usize + usize::from(len)])
            .unwrap()
    }

    #[test]
    fn shared_cells_cross_as_the_pinned_layout() {
        let engine = TerminalEngine::new(20, 3, 0).expect("engine");
        engine.write(b"\x1b[31mhi\x1b[0m".to_vec());
        let grid = engine.render_grid().expect("grid");
        assert_eq!(grid.cells.len(), 20 * 3 * 36);
        assert_eq!(text(&grid, 0), "h");
        assert_eq!(text(&grid, 1), "i");
        assert_eq!(grid.generation, 1);
    }

    #[test]
    fn predictive_echo_updates_the_pod_arena() {
        let engine = TerminalEngine::new(20, 4, 0).expect("engine");
        engine.write(b"$ ".to_vec());
        let _ = engine.render_grid().expect("settle");
        engine.predict_text("ls".to_owned());
        let grid = engine.render_grid().expect("predicted");
        assert_eq!(text(&grid, 2), "l");
        assert_eq!(text(&grid, 3), "s");
    }
}
