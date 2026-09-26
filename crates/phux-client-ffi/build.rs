fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // This rlib contributes declarations to its embedding addon's NAPI build.
    // Switching host features changes the metadata directory even when this
    // crate's Rust inputs stay cached. Regenerate instead of omitting its API.
    #[cfg(feature = "napi")]
    {
        println!("cargo::rerun-if-env-changed=NAPI_TYPE_DEF_TMP_FOLDER");
        println!("cargo::rerun-if-env-changed=NAPI_FORCE_BUILD_PHUX_CLIENT_FFI");
    }
}
