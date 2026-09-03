//! Frontend-neutral client session and pane-interior substrate.
//!
//! This crate owns synchronous terminal-session state and the reusable client
//! policies that must compile unchanged for native and wasm frontends:
//!
//! - [`engine`] and [`session`] — the generic terminal adapter and synchronous
//!   protocol-0.7 session kernel.
//! - [`layout`] — the pane-geometry layout tree, split math, and the CBOR
//!   metadata envelope that persists it server-side.
//! - [`multi_pane`] — layout tree → per-pane rectangles + the divider
//!   cells between them (pure compute; the chrome layer rasterizes the
//!   `DividerCell`s to VT).
//! - [`predict`] — Mosh-class predictive local echo over the pane mirror.
//!
//! # Frontend boundary
//!
//! Transport, async execution, timers, rendering, clipboard delivery, and
//! frontend event parsing stay in host crates. This crate carries no
//! `ratatui`, `crossterm`, `tokio`, `web-sys`, or DOM dependency. The kernel
//! exposes borrowed adapter-owned replicas and declarative effects; hosts
//! execute those effects without re-entering an update.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod engine;
pub mod history;
pub mod layout;
pub mod multi_pane;
pub mod perf;
pub mod predict;
pub mod session;
