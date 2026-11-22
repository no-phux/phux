//! The phux mobile binding crate: the `UniFFI` Swift and Kotlin surface over
//! the client runtime (ADR-0135).
//!
//! `projection` derives the product vocabulary from `phux-client-runtime`
//! in binding-neutral Rust; `crate::uniffi` marshals it.

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

mod projection;

// `UniFfiTag` must live at the crate root, where every `uniffi` derive
// names it. No namespace argument: the default (this crate's name) matches
// the archive, cdylib and Kotlin `loadLibrary` name.
::uniffi::setup_scaffolding!();

mod uniffi;
