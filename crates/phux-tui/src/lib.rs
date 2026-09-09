//! phux reference TUI.
//!
//! The interactive front end over the headless [`phux_client`] library:
//! `phux attach` connects through `phux_client::attach::connection`, then
//! this crate takes over the controlling terminal, feeds `RESOURCE_OUTPUT`
//! bytes into a local `libghostty_vt::Terminal` per attached pane, and
//! paints dirty rows plus the chrome back out to the outer terminal.
//!
//! See ADR-0013 for the bytes-on-wire decision, ADR-0020 for the two-layer
//! render model, and ADR-0100 for why the TUI is its own crate.
//!
//! # Render layering (epic phux-5ke, ADR-0020)
//!
//! Pane interiors are painted by libghostty (VT bytes -> `Terminal` ->
//! stdout). Chrome -- status bar, pane dividers, borders, overlays -- is
//! painted by `ratatui` from the [`render`] module. The two layers composite
//! over disjoint screen regions, never interleaved. `ratatui` lives only in
//! this crate; the pane-interior substrate (layout math, multi-pane
//! composition, predictive echo, the session kernel) lives in
//! `phux-client-core`, which carries no `ratatui` dependency, and the
//! headless control-plane client lives in `phux-client`, which carries
//! neither `ratatui` nor a terminal. Both boundaries are enforced by the
//! compiler: a stray `use ratatui` in either crate fails to build.
//!
//! The substrate modules are re-exported here ([`layout`], [`multi_pane`],
//! [`predict`]) so the driver keeps its `crate::{layout, predict, ...}`
//! paths and embedders can name them through one crate.
//!
//! # Features
//!
//! `native-engine` (default) enables the client-core replica host that
//! bootstraps panes from an exact native checkpoint (ADR-0070). `testkit`
//! turns on `phux_client::testkit`, the scripted server the driver's unit
//! tests speak to, for downstream crates that want the same harness.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod attach;
pub mod render;
// phux-u1tq.2: the config-derived TUI state, built once per attach and
// swapped whole on reload.
pub mod settings;

/// The shared benchmark corpora (`benchmarks/support.rs`), compiled into the
/// test build so a gate can assert against the same grids the benches
/// measure instead of a private copy that drifts.
///
/// Declared HERE rather than beside its only consumer
/// (`attach::render::tests`) because `#[path]` on a module nested inside
/// inline modules resolves against a directory that does not exist on disk
/// (`src/attach/render/tests/`), and a relative path cannot traverse `..`
/// through a directory that is not there. `src/` is real, so the path
/// resolves.
#[cfg(test)]
#[path = "../../../benchmarks/support.rs"]
pub(crate) mod bench_support;

// Pane-interior substrate, re-exported from `phux-client-core` so the
// `ratatui`-free boundary is compiler-enforced (ADR-0020) while the driver
// keeps stable `crate::{layout, multi_pane, predict}` paths.
pub use phux_client_core::{layout, multi_pane, predict};
