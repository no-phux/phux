//! Ensures the embedded engine module exists before `include_bytes!` runs, with
//! a clear remediation message instead of a raw "file not found".

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let wasm = std::path::Path::new(&manifest).join("vendor/ghostty-vt.wasm");
    if !wasm.exists() {
        panic!(
            "missing {} — run scripts/build-vt-wasm.sh (builds the checkpoint-capable standalone ghostty-vt.wasm)",
            wasm.display()
        );
    }
    println!("cargo:rerun-if-changed=vendor/ghostty-vt.wasm");
}
