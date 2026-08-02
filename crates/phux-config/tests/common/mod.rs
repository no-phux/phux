//! Shared helpers for the phux-config integration tests.
//!
//! Each test binary compiles its own copy of this module and uses only a
//! subset of it, so unused items are expected.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::expect_used,
    reason = "shared test support"
)]

use std::fs;
use std::path::{Path, PathBuf};

/// Nominal config path for parse-only tests that never touch the disk.
pub fn path() -> PathBuf {
    PathBuf::from("config.toml")
}

/// Write `contents` to `dir/name`, creating parent directories.
pub fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(&path, contents).expect("write file");
    path
}

/// A plugin-manifest body: the four required header keys plus `extra`.
///
/// `extra` is appended verbatim after the header, so it may add further
/// top-level keys before introducing any `[[table]]` sections.
pub fn manifest(id: &str, extra: &str) -> String {
    format!(
        "id = \"{id}\"\n\
         name = \"Test\"\n\
         version = \"0.1.0\"\n\
         min_phux_version = \"0.0.2\"\n\
         {extra}"
    )
}

/// Write a plugin manifest into `dir` and return its path.
pub fn write_manifest(dir: &Path, body: &str) -> PathBuf {
    write(dir, "phux-plugin.toml", body)
}
