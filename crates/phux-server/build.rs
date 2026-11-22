//! Build script: declare expected cfgs.
//!
//! `telemetry.rs` reads `#[cfg(tokio_unstable)]` to decide whether the
//! `tokio-console` layer is safe to install. The cfg is never set here —
//! it is supplied externally via `RUSTFLAGS="--cfg tokio_unstable"` when
//! building for tokio-console. This line just tells rustc the name is
//! expected so `#[cfg(tokio_unstable)]` does not trip the
//! `unexpected_cfgs` lint (denied via `-D warnings` in CI).
fn main() {
    println!("cargo::rustc-check-cfg=cfg(tokio_unstable)");
}
