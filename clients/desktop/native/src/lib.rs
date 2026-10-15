//! The phux desktop: a native GPUI + GPUI Kit application over the shared
//! client runtime (ADR-0165).
//!
//! Layers, outermost first:
//!
//! - [`launch`]: process entry. CLI options, installed-CLI resolution, log
//!   file, platform and Kit initialization, evidence and perf sinks.
//! - [`app`]: windows, commands, chrome and overlays.
//! - [`attach`]: dropped files and clipboard images as pasteable paths.
//! - [`workspace`] / [`settings`]: tabs, splits, persistence and preferences,
//!   with no GPUI types so they test without a window.
//! - [`connection`]: one runtime [`phux_client_runtime::Client`] per window and
//!   its sole event drain.
//! - [`terminal`] / [`input`] / [`presentation`]: the native grid painter, its
//!   input handler, and drawable-presented fence receipts.
//! - [`perf`] / [`diag`]: always-on host metrics, the log, `perf.jsonl`, and
//!   drawable-presented evidence for qualification runs.

pub mod app;
pub mod attach;
pub mod chrome;
pub mod connection;
pub mod diag;
pub mod fonts;
pub mod hotkeys;
pub mod input;
pub mod launch;
pub mod perf;
mod presentation;
pub mod settings;
pub mod telemetry;
pub mod terminal;
mod updates;
pub mod workspace;
