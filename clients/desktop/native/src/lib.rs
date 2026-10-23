//! The phux desktop: a native GPUI + GPUI Kit application over the shared
//! client runtime (ADR-0165).
//!
//! Layers, outermost first:
//!
//! - [`launch`]: process entry. CLI options, installed-CLI resolution, log
//!   file, platform and Kit initialization, evidence and perf sinks.
//! - [`app`]: windows, commands, chrome and overlays.
//! - [`workspace`] / [`settings`]: tabs, splits, persistence and preferences,
//!   with no GPUI types so they test without a window.
//! - [`connection`]: one runtime [`phux_client_runtime::Client`] per window and
//!   its sole event drain.
//! - [`terminal`] / [`input`] / [`attach`] / [`fonts`]: the embeddable
//!   terminal view from [`phux_gpui`], re-exported under their old paths.
//!   Native-owned agent guardians refuse terminal input instead of falling
//!   back from ACP to a PTY.
//! - [`perf`] / [`diag`]: always-on host metrics, the log, `perf.jsonl`, and
//!   drawable-presented evidence for qualification runs.
//! - [`devtools`]: Developer mode (the setting, `PHUX_DEV=1` or `--dev`):
//!   the frame ring, heap accounting, the HUD, Perfetto traces and the dev
//!   socket agents query.

pub use phux_gpui::{attach, fonts, input, perf, terminal};

pub mod app;
pub mod assets;
pub mod chrome;
pub mod connection;
pub mod devtools;
pub mod diag;
pub mod git;
pub mod hotkeys;
pub mod launch;
pub mod settings;
pub mod telemetry;
mod updates;
pub mod workspace;
