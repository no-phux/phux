//! The phux binding crate: one projection of the client runtime, one encoder
//! per foreign language (ADR-0135).
//!
//! [`projection`] derives the product vocabulary from `phux-client-runtime`
//! once, in binding-neutral Rust. The encoders sit behind features and are
//! mechanical over it:
//!
//! - `c-abi` (default) is `crate::c`: the stable `extern "C"` surface
//!   `include/phux/client.h` declares, which Cockpit links as a staticlib.
//! - `uniffi` is `crate::uniffi`: the Swift and Kotlin mobile surface.
//! - `napi` is `crate::napi`: the optional desktop encoder.

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

pub mod projection;

/// Optional desktop encoder and same-binary native client registry.
#[cfg(feature = "napi")]
pub mod napi;

#[cfg(feature = "c-abi")]
mod c;
#[cfg(feature = "c-abi")]
pub use c::*;

// `UniFfiTag` must live at the crate root, where every `uniffi` derive
// names it. No namespace argument: the default (this crate's name) matches
// the archive, cdylib and Kotlin `loadLibrary` name.
#[cfg(feature = "uniffi")]
::uniffi::setup_scaffolding!();

#[cfg(feature = "uniffi")]
mod uniffi;
