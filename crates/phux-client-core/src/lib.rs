//! Frontend-neutral client session and pane-interior substrate.
//!
//! This crate owns synchronous terminal-session state and the reusable client
//! policies that must compile unchanged for native and wasm frontends:
//!
//! - [`engine`] and [`session`] — the generic terminal adapter and synchronous
//!   protocol-0.7 session kernel.
//! - `grid` (feature `native-engine`) — the one plain-old-data cell layout
//!   and the projector that flattens a libghostty viewport into it
//!   (ADR-0133); every binding lends or copies this buffer.
//! - [`handshake`] — `HELLO_OK` acceptance shared by every frontend.
//! - [`rename`] — the session-rename decision, write, and `GET_STATE`
//!   barrier shared by every frontend.
//! - [`layout`] — the pane-geometry layout tree, split math, and the CBOR
//!   metadata envelope that persists it server-side.
//! - [`multi_pane`] — layout tree → per-pane rectangles + the divider
//!   cells between them (pure compute; the chrome layer rasterizes the
//!   `DividerCell`s to VT).
//! - [`predict`] — Mosh-class predictive local echo over the pane mirror.
//! - [`input_replay`] — acknowledged input ordering and reconnect policy.
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
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub mod grid;
pub mod handshake;
pub mod history;
pub mod input_replay;
pub mod layout;
pub mod multi_pane;
pub mod perf;
pub mod predict;
pub mod rename;
pub mod session;
