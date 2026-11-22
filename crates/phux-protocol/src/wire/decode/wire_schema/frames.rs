//! Each `FrameKind` variant's schema name and index, from the frame catalog.

use super::FrameKind;

/// Number of [`FrameKind`] variants; [`frame_entry`] indexes them `0..`.
pub(super) const FRAME_VARIANTS: usize = FrameKind::VARIANTS;

/// `(variant index, spec name, field.rs module)` for one frame, from its
/// row in the frame catalog (`wire/frame/catalog.rs`).
pub(super) const fn frame_entry(frame: &FrameKind) -> (usize, &'static str, &'static str) {
    (frame.catalog_row(), frame.spec_name(), frame.field_module())
}

/// `FRAME_COMPRESSED` is no `FrameKind` variant: the envelope is sampled by
/// compressing one, so it is named here.
pub(super) const FRAME_COMPRESSED: (&str, &str) = ("FRAME_COMPRESSED", "frame_compressed");
