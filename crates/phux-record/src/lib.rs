//! Offline session-recording codec and exporter for phux (ADR-0060).
//!
//! Pure and synchronous: no tokio, no `phux-client`. Every entry point takes
//! an `impl Write` or `impl BufRead`, so the same code serves the live `--rec`
//! tee, headless `phux rec`, and an offline `--from cast -o gif` re-render.
//!
//! ```text
//!   bytes ─▶ cast (asciicast v2/v3) ─▶ timeline (idle clamp)
//!                                   ├▶ playback (wall-clock deadlines)
//!                                   └▶ replay ─▶ raster ─▶ encode (GIF/APNG)
//!                                       driven by render
//! ```
//!
//! # Features
//!
//! * `render` (default): the export pipeline. Pulls `libghostty-vt`, `png`,
//!   `gif`, and `phux-protocol` with only `render-pool` (ADR-0086).
//! * without it: only [`cast`], [`timeline`], [`playback`], and [`error`].
//!   `phux-client` takes this shape so the TUI compiles no image encoders or
//!   second terminal emulator.
//!
//! Everything internal is integer milliseconds from session start
//! ([`cast::CastEvent::time_ms`]), so no stage accumulates float drift.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "pub(crate) in private modules keeps unreachable_pub quiet"
)]

pub mod cast;
pub mod error;
/// Live marker inbox shared by `phux rec mark` and the `rec-mark` hotkey.
pub mod markers;
pub mod playback;
pub mod timeline;

#[cfg(feature = "render")]
mod encode;
#[cfg(feature = "render")]
mod font;
#[cfg(feature = "render")]
mod raster;
#[cfg(feature = "render")]
pub mod render;
#[cfg(feature = "render")]
mod replay;

pub use cast::{CastEvent, CastHeader, CastTheme, CastVersion, CastWriter, EventCode, read_cast};
pub use error::RecordError;
pub use markers::{MarkInbox, post_mark, post_mark_in, recording_live};
pub use playback::{Speed, due_at, pass_duration};
pub use timeline::{clamp_idle, compress_marked};

/// Pixel width of the vendored recording face (Spleen 8x16, ADR-0060).
///
/// A composited capture's `--size` is in these pixels: the cast grid is
/// `width / FACE_CELL_WIDTH_PX` by `height / FACE_CELL_HEIGHT_PX` cells.
pub const FACE_CELL_WIDTH_PX: u32 = 8;
/// Pixel height of the vendored recording face.
pub const FACE_CELL_HEIGHT_PX: u32 = 16;

/// Cell grid for a pixel viewport, floored to the face and at least 1x1.
///
/// `1920x1080` is 240 columns by 67 rows (`1080 / 16` floors).
#[must_use]
pub fn cells_for_pixels(width_px: u32, height_px: u32) -> (u16, u16) {
    let cols = (width_px / FACE_CELL_WIDTH_PX).clamp(1, u32::from(u16::MAX));
    let rows = (height_px / FACE_CELL_HEIGHT_PX).clamp(1, u32::from(u16::MAX));
    (
        u16::try_from(cols).unwrap_or(u16::MAX),
        u16::try_from(rows).unwrap_or(u16::MAX),
    )
}

/// Parse `WIDTHxHEIGHT` pixels. Zero and non-numeric sizes are rejected.
/// The separator is `x` or `X`.
#[must_use]
pub fn parse_pixel_size(text: &str) -> Option<(u32, u32)> {
    let text = text.trim();
    let (width, height) = text.split_once('x').or_else(|| text.split_once('X'))?;
    let width: u32 = width.parse().ok()?;
    let height: u32 = height.parse().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

#[cfg(feature = "render")]
pub use render::{OutputFormat, RenderOptions, RenderStats, render_cast};
#[cfg(feature = "render")]
pub use replay::{Replayer, Sampled};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod pixel_tests {
    use super::{cells_for_pixels, parse_pixel_size};

    #[test]
    fn a_1080p_viewport_is_240_by_67_cells() {
        assert_eq!(parse_pixel_size("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_pixel_size(" 1920X1080 "), Some((1920, 1080)));
        assert_eq!(cells_for_pixels(1920, 1080), (240, 67));
    }

    #[test]
    fn a_zero_or_garbage_size_is_rejected_and_a_tiny_one_is_one_cell() {
        assert_eq!(parse_pixel_size("0x1080"), None);
        assert_eq!(parse_pixel_size("1920"), None);
        assert_eq!(parse_pixel_size("axb"), None);
        assert_eq!(cells_for_pixels(1, 1), (1, 1));
    }
}
