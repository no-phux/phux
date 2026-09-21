//! The phux binding crate: one projection of the client runtime, one encoder
//! per foreign language (ADR-0135).
//!
//! [`projection`] derives the product vocabulary — terminal signals,
//! lifecycle answers, topology, status, delivery outcomes, receipts, agent
//! records and grid frames — from `phux-client-runtime` values exactly once,
//! in binding-neutral Rust. Nothing in it is `#[repr(C)]` and nothing in it
//! carries a `uniffi` derive.
//!
//! The encoders sit behind features and are mechanical over that layer:
//!
//! - `c-abi` (default) is `crate::c`: the stable `extern "C"` surface
//!   `include/phux/client.h` declares, which Cockpit links as a staticlib.
//! - `uniffi` is `crate::uniffi`: the `RemoteClient` object, the local
//!   playground engine, the keymap and the predictor that `UniFFI` turns into
//!   Swift and Kotlin for the native mobile clients.
//!
//! Both lanes build from the same crate and the same lockfile, so a runtime
//! value cannot mean one thing to Cockpit and another to the phone.

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

pub mod projection;

#[cfg(feature = "c-abi")]
mod c;
#[cfg(feature = "c-abi")]
pub use c::*;

// The UniFFI scaffolding's `UniFfiTag` must live at the crate root: every
// `uniffi` derive in `crate::uniffi` names `crate::UniFfiTag`. The leading
// `::` is required because the module below shadows the crate name here.
//
// No namespace argument: UniFFI defaults it to this crate's own name, so the
// generated Kotlin file is `phux_client_ffi.kt` and its `loadLibrary` call
// asks for `phux_client_ffi`, matching the archive and cdylib cargo emits.
// One crate, one name, everywhere the artifact is read (ADR-0135).
#[cfg(feature = "uniffi")]
::uniffi::setup_scaffolding!();

#[cfg(feature = "uniffi")]
mod uniffi;
