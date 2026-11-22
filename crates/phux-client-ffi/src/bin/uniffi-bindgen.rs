//! Binding generator entry point. Invoked by `just mobile-ffi-xcframework`:
//!
//! ```text
//! cargo run --bin uniffi-bindgen -- generate \
//!   --library target/<triple>/release/libphux_client_ffi.dylib \
//!   --language swift --out-dir <PhuxFFI/Generated>
//! ```
//!
//! Library mode reads the exported surface straight from the built binary, so
//! there is no UDL to keep in sync.
fn main() {
    uniffi::uniffi_bindgen_main();
}
