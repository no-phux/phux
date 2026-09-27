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
pub use playback::{Speed, due_at, pass_duration};
pub use timeline::clamp_idle;

#[cfg(feature = "render")]
pub use render::{OutputFormat, RenderOptions, RenderStats, render_cast};
#[cfg(feature = "render")]
pub use replay::{Replayer, Sampled};
