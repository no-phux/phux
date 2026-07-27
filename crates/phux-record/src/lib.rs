//! Offline session-recording codec and exporter for phux.
//!
//! This crate is the *export* half of built-in session recording (ADR-0060).
//! It is deliberately pure, offline, and synchronous: no tokio, no
//! `phux-protocol`, no `phux-client`. Every entry point takes an
//! `impl std::io::Write` or an `impl std::io::BufRead`, so the same code
//! serves the live `--rec` tee (which streams into a file as the session
//! runs), the headless `phux rec` capture, and a pure `--from cast -o gif`
//! re-render with no server anywhere in the picture.
//!
//! The pipeline is one-way, and no stage knows about the stage above it:
//!
//! ```text
//!   captured bytes ──▶ [`cast`]   asciicast v2/v3 read + write
//!                        │
//!                        ├──────▶ [`timeline`] idle clamping on a ms timebase
//!                        │
//!                        └──────▶ [`replay`]   bytes  -> RenderedFrame
//!                                    │
//!                                    ├───────▶ [`raster`] frame -> RGB surface
//!                                    │
//!                                    └───────▶ [`encode`] surface -> GIF / APNG
//!                                                 driven by [`render`]
//! ```
//!
//! # Features
//!
//! * `render` (default) — the whole export pipeline: [`replay`], [`raster`],
//!   [`encode`], [`render`], [`font`]. Pulls `libghostty-vt`, `png`, `gif`.
//! * default off — only [`cast`], [`timeline`], and [`error`]. This is the
//!   shape `phux-client` takes (`default-features = false`): the TUI needs to
//!   *write* a cast while a session runs and must not compile image encoders
//!   or a second terminal emulator to do it.
//!
//! # Timebase
//!
//! Everything internal is **integer milliseconds, absolute from session
//! start** ([`cast::CastEvent::time_ms`]). Serialization converts on the way
//! out — v2 writes absolute seconds, v3 writes relative intervals — so
//! neither format can accumulate float drift, and the `.cast` and the GIF
//! rendered from it can never disagree about when something happened.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod cast;
pub mod error;
pub mod timeline;

#[cfg(feature = "render")]
pub mod encode;
#[cfg(feature = "render")]
pub mod font;
#[cfg(feature = "render")]
pub mod raster;
#[cfg(feature = "render")]
pub mod render;
#[cfg(feature = "render")]
pub mod replay;

pub use cast::{CastEvent, CastHeader, CastTheme, CastVersion, CastWriter, EventCode, read_cast};
pub use error::RecordError;
pub use timeline::clamp_idle;

#[cfg(feature = "render")]
pub use raster::{Rasterizer, Surface, Theme};
#[cfg(feature = "render")]
pub use render::{OutputFormat, RenderOptions, RenderStats, render_cast};
#[cfg(feature = "render")]
pub use replay::{Replayer, Sampled};
