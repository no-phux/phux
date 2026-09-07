//! Additive render metadata for the unchanged v1 grid/cell ABI.
//!
//! Captured in the same render pass as the dense grid; the read-only query
//! neither renders again nor invalidates that grid's borrowed pointers.

use std::{mem::size_of, ptr};

use libghostty_vt::render::{Colors, Snapshot};
use libghostty_vt::screen::{CellContentTag, CellWide};
use libghostty_vt::style::{RgbColor, Style, StyleColor};
use libghostty_vt::terminal::{Mode, Terminal};

use crate::error::{BridgeError, check_struct, terminal_id_in};
use crate::{
    ABI_VERSION, PhuxClient, PhuxClientResult, PhuxTerminalCell, PhuxTerminalGridView,
    PhuxTerminalId, with_client_ref,
};

pub const GRID_COLOR_DEFAULT: u8 = 0;
pub const GRID_COLOR_PALETTE: u8 = 1;
pub const GRID_COLOR_RGB: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhuxGridRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<RgbColor> for PhuxGridRgb {
    fn from(value: RgbColor) -> Self {
        Self {
            r: value.r,
            g: value.g,
            b: value.b,
        }
    }
}

/// Color provenance lost by the v1 cell's palette-resolved RGB fields.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PhuxGridCellMetadata {
    pub foreground_kind: u8,
    /// Meaningful only for `GRID_COLOR_PALETTE`.
    pub foreground_palette_index: u8,
    pub underline_color_is_default: bool,
    pub background_color_is_default: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxTerminalGridMetadata {
    pub size: usize,
    pub version: u32,
    pub stream_id: u64,
    pub bootstrap_id: u64,
    pub last_seq: u64,
    pub document_revision: u64,
    pub cols: u16,
    pub rows: u16,
    pub foreground: PhuxGridRgb,
    pub background: PhuxGridRgb,
    pub cursor_color: PhuxGridRgb,
    pub has_foreground: bool,
    pub has_background: bool,
    pub reverse_colors: bool,
    pub has_cursor_color: bool,
    pub cursor_blinking: bool,
    pub cursor_wide: bool,
    pub cursor_at_wide_tail: bool,
    pub palette: [PhuxGridRgb; 256],
    pub cells: *const PhuxGridCellMetadata,
    pub cell_count: usize,
}

impl Default for PhuxTerminalGridMetadata {
    fn default() -> Self {
        Self {
            size: size_of::<Self>(),
            version: ABI_VERSION,
            stream_id: 0,
            bootstrap_id: 0,
            last_seq: 0,
            document_revision: 0,
            cols: 0,
            rows: 0,
            foreground: PhuxGridRgb::default(),
            background: PhuxGridRgb::default(),
            cursor_color: PhuxGridRgb::default(),
            has_foreground: false,
            has_background: false,
            reverse_colors: false,
            has_cursor_color: false,
            cursor_blinking: false,
            cursor_wide: false,
            cursor_at_wide_tail: false,
            palette: [PhuxGridRgb::default(); 256],
            cells: ptr::null(),
            cell_count: 0,
        }
    }
}

#[derive(Default)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "private cache shared with client flattening, not a C API export"
)]
pub(crate) struct GridMetadataCache {
    pub cells: Vec<PhuxGridCellMetadata>,
    pub view: PhuxTerminalGridMetadata,
    pub valid: bool,
}

impl GridMetadataCache {
    pub(crate) fn publish(
        &mut self,
        snapshot: &Snapshot<'_, '_>,
        terminal: &Terminal<'_, '_>,
        colors: &Colors,
        grid: &PhuxTerminalGridView,
        cells: &[PhuxTerminalCell],
    ) -> Result<(), BridgeError> {
        let cursor = snapshot.cursor_viewport().map_err(BridgeError::ghostty)?;
        let wide_tail = cursor.is_some_and(|value| value.at_wide_tail);
        let cursor_index =
            usize::from(grid.cursor_row) * usize::from(grid.cols) + usize::from(grid.cursor_col);
        let wide = cells
            .get(cursor_index)
            .is_some_and(|cell| cell.wide == CellWide::Wide as u8);
        let defaults = DefaultColors::read(terminal)?;
        self.view = PhuxTerminalGridMetadata {
            stream_id: grid.stream_id,
            bootstrap_id: grid.bootstrap_id,
            last_seq: grid.last_seq,
            document_revision: grid.document_revision,
            cols: grid.cols,
            rows: grid.rows,
            foreground: defaults
                .foreground
                .map_or_else(PhuxGridRgb::default, Into::into),
            background: defaults
                .background
                .map_or_else(PhuxGridRgb::default, Into::into),
            has_foreground: defaults.foreground.is_some(),
            has_background: defaults.background.is_some(),
            reverse_colors: defaults.reversed,
            cursor_color: colors.cursor.map_or_else(PhuxGridRgb::default, Into::into),
            has_cursor_color: colors.cursor.is_some(),
            cursor_blinking: snapshot.cursor_blinking().map_err(BridgeError::ghostty)?,
            cursor_wide: grid.cursor_visible && (wide || wide_tail),
            cursor_at_wide_tail: wide_tail,
            palette: colors.palette.map(Into::into),
            cells: self.cells.as_ptr(),
            cell_count: self.cells.len(),
            ..PhuxTerminalGridMetadata::default()
        };
        self.valid = true;
        Ok(())
    }
}

#[allow(
    clippy::redundant_pub_crate,
    reason = "private helper shared with client flattening, not a C API export"
)]
pub(crate) const fn cell_metadata(style: Style, content: CellContentTag) -> PhuxGridCellMetadata {
    let (foreground_kind, foreground_palette_index) = match style.fg_color {
        StyleColor::None => (GRID_COLOR_DEFAULT, 0),
        StyleColor::Palette(index) => (GRID_COLOR_PALETTE, index.0),
        StyleColor::Rgb(_) => (GRID_COLOR_RGB, 0),
    };
    PhuxGridCellMetadata {
        foreground_kind,
        foreground_palette_index,
        underline_color_is_default: matches!(style.underline_color, StyleColor::None),
        background_color_is_default: default_background(style, content),
    }
}

const fn default_background(style: Style, content: CellContentTag) -> bool {
    if matches!(
        content,
        CellContentTag::BgColorPalette | CellContentTag::BgColorRgb
    ) {
        return false;
    }
    matches!(style.bg_color, StyleColor::None)
}

struct DefaultColors {
    foreground: Option<RgbColor>,
    background: Option<RgbColor>,
    reversed: bool,
}

impl DefaultColors {
    fn read(terminal: &Terminal<'_, '_>) -> Result<Self, BridgeError> {
        // RenderState retains its previous colors if either terminal default
        // is unset. Reading the terminal's effective option avoids exporting
        // stale OSC colors after reset; the embedder supplies its theme fallback.
        let mut foreground = terminal.fg_color().map_err(BridgeError::ghostty)?;
        let mut background = terminal.bg_color().map_err(BridgeError::ghostty)?;
        let reversed = terminal
            .mode(Mode::REVERSE_COLORS)
            .map_err(BridgeError::ghostty)?;
        if reversed {
            std::mem::swap(&mut foreground, &mut background);
        }
        Ok(Self {
            foreground,
            background,
            reversed,
        })
    }
}

/// Read metadata for the last grid built for this terminal.
///
/// Returns `InvalidState` before a grid is built or after any mutable client call.
/// Initialize `out_metadata.size` and `.version` before calling. This query
/// leaves both the grid and metadata borrows valid.
///
/// # Safety
///
/// `client` must be live on its owning thread and unmodified during the call.
/// Non-null `terminal_id` and its host span must be readable. Non-null
/// `out_metadata` must be independent readable/writable storage for its size
/// and version, and for the whole struct when those fields pass validation.
/// Returned cell metadata remains borrowed until the next mutable client call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_grid_metadata(
    client: *const PhuxClient,
    terminal_id: *const PhuxTerminalId,
    out_metadata: *mut PhuxTerminalGridMetadata,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // Read only the header until its caller-provided storage size is checked.
        if out_metadata.is_null() {
            return Err(BridgeError::invalid("out_metadata is null"));
        }
        // SAFETY: the caller promises readable size/version fields, even for
        // undersized output storage. No reference to the full struct is made.
        let (size, version) = unsafe {
            (
                ptr::addr_of!((*out_metadata).size).read(),
                ptr::addr_of!((*out_metadata).version).read(),
            )
        };
        check_struct(size, size_of::<PhuxTerminalGridMetadata>(), version)?;
        // SAFETY: validated full-sized independent output storage.
        unsafe { out_metadata.write(PhuxTerminalGridMetadata::default()) };
        // SAFETY: forwards the caller's terminal-id span contract.
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let cache = client
            .render
            .get(&terminal_id)
            .filter(|cache| cache.metadata.valid)
            .ok_or_else(|| BridgeError::state("grid metadata requires a current borrowed grid"))?;
        // SAFETY: output is validated above; the cached view borrows client-owned storage.
        unsafe { out_metadata.write(cache.metadata.view) };
        Ok(())
    })
}
