//! The phux binding crate: one projection of the client runtime, one encoder
//! per foreign language (ADR-0135).
//!
//! [`projection`] derives the product vocabulary from `phux-client-runtime`
//! once, in binding-neutral Rust. The encoders sit behind features and are
//! mechanical over it:
//!
//! - `c-abi` (default) is `crate::c`: the stable `extern "C"` surface
//!   `include/phux/client.h` declares, available as a native staticlib.
//! - `uniffi` is `crate::uniffi`: the Swift and Kotlin mobile surface.
//!
//! The native desktop app is Rust, so it needs no encoder: it builds this crate
//! with default features off and reads [`projection`] directly.

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

pub mod projection;

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
