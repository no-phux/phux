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
// The namespace argument is `phux_mobile_ffi`, not this crate's own name:
// UniFFI's library-mode bindgen names the generated Kotlin file and the
// cdylib `loadLibrary` call after it (`{namespace}.kt`), so this is what
// keeps `phux-mobile`'s generated file and `.so` byte-for-byte unchanged
// from the `phux-mobile-ffi` era (ADR-0135) despite the crate rename. The
// low-level FFI symbol names UniFFI generates are keyed off the Rust module
// path instead, so this has no effect on linkage.
#[cfg(feature = "uniffi")]
::uniffi::setup_scaffolding!("phux_mobile_ffi");

#[cfg(feature = "uniffi")]
mod uniffi;
