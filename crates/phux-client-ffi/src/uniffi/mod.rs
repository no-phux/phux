#![allow(
    unreachable_pub,
    clippy::missing_const_for_fn,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::redundant_pub_crate,
    reason = "UniFFI exports are public outside Rust and require owned, non-const signatures"
)]

//! The `UniFFI` encoder: the Swift and Kotlin surface, over
//! [`crate::projection`].
//!
//! `phux-client-runtime` owns transport, reconnect, the control plane and the
//! connected-client engine. The projection layer decides what its values
//! mean. This module only lowers those decisions into the vocabulary `UniFFI`
//! can carry across the language boundary, and holds the small amount of
//! binding-local state a polling consumer needs (the pending metadata reads,
//! the per-pane generation the last render observed).
//!
//! The isolated [`engine`] module also serves local playground and test
//! terminals that have no connection and no runtime session at all.
//!
//! Before ADR-0135 this was a separate crate, `phux-mobile-ffi`, with its own
//! hand-written projection of the same runtime. One crate, one projection,
//! two encoders replaced it.

// The VT engine grid surface (ADR-0007), and the local playground terminal
// built on it.
mod engine;

// Mobile text commits adapted onto the canonical shared predictor.
mod predict;

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

#[cfg(test)]
mod tests {
    use super::{bridge_ready, engine_version, protocol_version};

    #[test]
    fn version_is_semver_shaped() {
        let v = protocol_version();
        let parts: Vec<&str> = v.split('.').collect();
        assert_eq!(parts.len(), 3, "expected major.minor.patch, got {v:?}");
        assert!(parts.iter().all(|p| p.parse::<u16>().is_ok()));
    }

    #[test]
    fn engine_version_is_the_crate_version() {
        assert_eq!(engine_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn bridge_is_ready() {
        assert!(bridge_ready());
    }
}
