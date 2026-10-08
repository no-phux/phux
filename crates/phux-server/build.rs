//! Build script: declare expected cfgs, and compile the fd-table trampoline.
//!
//! `telemetry.rs` reads `#[cfg(tokio_unstable)]` to decide whether the
//! `tokio-console` layer is safe to install. The cfg is never set here —
//! it is supplied externally via `RUSTFLAGS="--cfg tokio_unstable"` when
//! building for tokio-console. This line just tells rustc the name is
//! expected so `#[cfg(tokio_unstable)]` does not trip the
//! `unexpected_cfgs` lint (denied via `-D warnings` in CI).
//!
//! `phux_fd_shrink` is set when the host C compiler produced
//! `phux-fd-shrink`, the tiny executable a pane child runs to shrink its
//! fd table (Linux) and to establish the controlling terminal before
//! exec. A missing compiler degrades to portable-pty's own spawn.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(tokio_unstable)");
    println!("cargo::rustc-check-cfg=cfg(phux_fd_shrink)");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "linux" && os != "macos" {
        return;
    }

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        println!("cargo:warning=OUT_DIR unset; pane spawns keep the inherited fd table");
        return;
    };
    let out_dir = std::path::PathBuf::from(out_dir);
    let dest = out_dir.join("phux-fd-shrink");
    let src = "src/resource/terminal/fd_shrink.c";
    println!("cargo::rerun-if-changed={src}");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let compiled = std::process::Command::new(&cc)
        .arg("-O2")
        .arg("-o")
        .arg(&dest)
        .arg(src)
        .status();
    match compiled {
        Ok(status) if status.success() => {
            println!("cargo::rustc-cfg=phux_fd_shrink");
            println!("cargo::rustc-env=PHUX_FD_SHRINK_BIN={}", dest.display());
        }
        Ok(status) => {
            println!(
                "cargo:warning=phux-fd-shrink failed to compile ({status}); pane spawns keep the inherited fd table"
            );
        }
        Err(err) => {
            println!(
                "cargo:warning=phux-fd-shrink compiler `{cc}` unavailable ({err}); pane spawns keep the inherited fd table"
            );
        }
    }
}
