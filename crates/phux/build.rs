//! Forward the next-channel identity into the binary.
//!
//! Stable builds leave `PHUX_VERSION_LABEL` equal to the crate version.
//! The next-release workflow sets `PHUX_BUILD_CHANNEL=next` and
//! `PHUX_BUILD_SHA` to the git SHA so `--version` and `phux update --check`
//! can tell a next install from the last stable Cargo version.

fn main() {
    println!("cargo:rerun-if-env-changed=PHUX_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=PHUX_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");

    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| String::from("0.0.0"));
    let channel = std::env::var("PHUX_BUILD_CHANNEL").unwrap_or_default();
    let sha = std::env::var("PHUX_BUILD_SHA").unwrap_or_default();

    let label = if channel == "next" && is_git_sha(&sha) {
        let short = &sha[..7];
        println!("cargo:rustc-env=PHUX_BUILD_CHANNEL=next");
        println!("cargo:rustc-env=PHUX_BUILD_SHA={sha}");
        format!("{pkg}+next.{short}")
    } else {
        println!("cargo:rustc-env=PHUX_BUILD_CHANNEL=stable");
        pkg
    };
    println!("cargo:rustc-env=PHUX_VERSION_LABEL={label}");
}

fn is_git_sha(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}
