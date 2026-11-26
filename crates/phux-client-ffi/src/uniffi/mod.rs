#![allow(
    unreachable_pub,
    clippy::missing_const_for_fn,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::redundant_pub_crate,
    reason = "UniFFI exports are public outside Rust and require owned, non-const signatures"
)]

//! The `UniFFI` encoder: the Swift and Kotlin surface, over
//! [`crate::projection`] (ADR-0135).
//!
//! The runtime owns transport, reconnect, the control plane and the engine;
//! the projection layer decides what its values mean. This module only
//! lowers those decisions into `UniFFI` vocabulary and holds the little
//! binding-local state a polling consumer needs. The isolated [`engine`]
//! module serves connectionless playground and test terminals.

// The design language: theme solver, colour maths, status vocabulary, catalog.
mod design;

// The VT engine grid surface (ADR-0007), and the local playground terminal
// built on it.
mod engine;

// Exact local-owner archived-recipe review; separate from remote transports.
mod archived_relaunch;

// The remote bridge: one `RemoteClient` object over a runtime session.
mod wire;

// Key input mapping: Swift keypresses -> wire `KeyEvent`s. The native edition
// of phux-web's browser mapping; used by the wire bridge's send path.
mod keymap;

/// The phux wire protocol version this bridge was built against, formatted as
/// `"major.minor.patch"`.
#[uniffi::export]
pub fn protocol_version() -> String {
    let v = phux_protocol::PROTOCOL_VERSION;
    format!("{}.{}.{}", v.major, v.minor, v.patch)
}

/// The native bridge crate version embedded in this app build.
#[uniffi::export]
pub fn engine_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Liveness check for the FFI seam: returns true once Swift can call into Rust
/// at all. Exercised by `PhuxFFI`'s smoke test.
#[uniffi::export]
pub fn bridge_ready() -> bool {
    true
}
