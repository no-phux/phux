//! phux reference TUI over the headless [`phux_client`] library: one local
//! `libghostty_vt::Terminal` per pane (ADR-0013), chrome painted by `ratatui`
//! from [`render`] over disjoint regions (ADR-0020). `ratatui` lives only in
//! this crate; the pane substrate is re-exported as [`layout`],
//! [`multi_pane`], and [`predict`].

// Tokio's `AsyncFd` registration requires a caller-side lifetime proof. Keep
// unsafe denied except for the two audited registrations in `attach::tty_input`.
#![deny(unsafe_code)]
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

#[cfg(test)]
mod test_util;

// Pane-interior substrate, re-exported so the `ratatui`-free boundary stays
// compiler-enforced (ADR-0020).
pub use phux_client_core::{layout, multi_pane, predict};
