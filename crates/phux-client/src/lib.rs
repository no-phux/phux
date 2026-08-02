//! phux TUI client.
//!
//! Receives `TERMINAL_OUTPUT` byte frames from a phux server (see
//! `phux-protocol`), feeds them into a local `libghostty_vt::Terminal`
//! per attached pane, and renders dirty rows back out to the outer
//! terminal via `RenderState`. Knows nothing about PTYs.
//!
//! See ADR-0013 for the bytes-on-wire decision and
//! `research/2026-05-25-libghostty-renderstate.md` for the renderer-side
//! contract this crate implements.
//!
//! # Render layering (epic phux-5ke, ADR-0020)
//!
//! Pane interiors are painted by libghostty (VT bytes → `Terminal` →
//! stdout). Chrome — status bar, pane dividers, borders, overlays — is
//! painted by `ratatui` from the [`render`] module. The two layers
//! composite over disjoint screen regions, never interleaved. `ratatui`
//! lives only in this crate; the pane-interior substrate (layout math,
//! multi-pane composition, predictive echo) lives in the `phux-client-core`
//! crate, which carries no `ratatui` dependency. The boundary is therefore
//! enforced by the compiler — a stray `use ratatui` in the substrate fails
//! to build — rather than by the retired `check-ratatui-boundary.sh` grep.
//! See ADR-0020.
//!
//! The substrate modules are re-exported here ([`layout`], [`multi_pane`],
//! [`predict`]) so consumers keep their `phux_client::{layout, predict, …}`
//! paths.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod agent_meta;
pub mod ask;
pub mod attach;
pub mod explain;
pub mod l2;
pub mod layout_ops;
pub mod record;
pub mod render;
pub mod resize;
pub mod run;
pub mod selector;
pub mod send_keys;
pub mod snapshot;
pub mod state;
// The one scripted server every client-side test speaks to (phux-h5hj.3).
// Compiled for this crate's own unit tests, and behind the `testkit` feature
// for the downstream crates (`phux-mcp`, the `phux` binary) whose unit tests
// used to hand-roll their own fake — and therefore encoded whatever their
// author believed the server does.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
pub mod vcs;
pub mod wait;
pub mod watch;

// Pane-interior substrate, re-exported from `phux-client-core` so the
// `ratatui`-free boundary is compiler-enforced (ADR-0020) while consumers
// keep stable `phux_client::{layout, multi_pane, predict}` paths.
pub use phux_client_core::{layout, multi_pane, predict};
