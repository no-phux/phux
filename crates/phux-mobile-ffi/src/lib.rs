#![allow(
    unreachable_pub,
    clippy::missing_const_for_fn,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::redundant_pub_crate,
    reason = "UniFFI exports are public outside Rust and require owned, non-const signatures"
)]

//! phux-mobile FFI bridge.
//!
//! A thin, safe `UniFFI` projection over `phux-client-runtime` for native
//! mobile clients. The runtime owns transport, reconnect, control-plane, and
//! connected-client engine state; this crate only maps those values into the
//! Swift/Kotlin surface. The isolated `engine` module also serves local
//! playground and test terminals that have no connection or runtime session.
//! This crate is a sibling of Cockpit's stable `phux-client-ffi` C ABI, never a
//! wrapper around it (ADR-0133 and phux-mobile ADR-0031).

uniffi::setup_scaffolding!();

// The VT engine grid surface (ADR-0007). Compiled only with `--features engine`
// (needs ../phux's nix/zig toolchain); the default build omits it entirely.
#[cfg(feature = "engine")]
mod engine;

// Mobile text commits adapted onto the canonical shared predictor. This is
// engine-only because prediction belongs to the native projection owner.
#[cfg(feature = "engine")]
mod predict;

// The remote WebSocket wire bridge (M2, ADR-0009). The `wire` feature uses a
// bounded headless kernel adapter; `wire,engine` swaps in the owning-thread
// Ghostty adapter and projects grids directly to Swift.
#[cfg(feature = "wire")]
mod wire;

// Key input mapping: Swift keypresses -> wire `KeyEvent`s. The native edition
// of phux-web's browser mapping; used by the wire bridge's send path.
#[cfg(feature = "wire")]
mod keymap;

/// The phux wire protocol version this bridge was built against, formatted as
/// `"major.minor.patch"`.
///
/// Read live from `phux-protocol`'s `PROTOCOL_VERSION`. Before phux-mobile
/// ADR-0031 this crate lived outside the phux workspace and mirrored the
/// constant behind a `phux-source` feature so it could build without a phux
/// checkout; beside the runtime there is nothing to mirror.
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
    use super::*;

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
