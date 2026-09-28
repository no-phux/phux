//! phux reference TUI: the interactive front end over the headless
//! [`phux_client`] library. It takes over the controlling terminal, feeds
//! `RESOURCE_OUTPUT` bytes into a local `libghostty_vt::Terminal` per pane
//! (ADR-0013), and paints dirty rows plus chrome back out.
//!
//! Pane interiors are painted by libghostty; chrome (status bar, dividers,
//! overlays) by `ratatui` from [`render`], over disjoint regions (ADR-0020).
//! `ratatui` lives only in this crate: the pane-interior substrate
//! (`phux-client-core`, re-exported as [`layout`], [`multi_pane`],
//! [`predict`]) and the headless client (`phux-client`) cannot link it.
//! `testkit` turns on `phux_client::testkit` for downstream tests.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod attach;
pub mod render;
/// Local bug-report bundles: session, logs, and a screen dump an agent can open.
pub mod report;
// The config-derived TUI state, built once per attach and
// swapped whole on reload.
pub mod settings;

/// The shared benchmark corpora, compiled into the test build so gates
/// assert against the grids the benches measure. Declared here because
/// `#[path]` inside nested inline modules resolves against a directory
/// that does not exist.
#[cfg(test)]
#[path = "../../../benchmarks/support.rs"]
pub(crate) mod bench_support;

// Pane-interior substrate, re-exported so the `ratatui`-free boundary stays
// compiler-enforced (ADR-0020).
pub use phux_client_core::{layout, multi_pane, predict};
