//! Config scaffolding: a commented starter `config.toml`.
//!
//! The scaffold is a comment-projection of [`DEFAULT_CONFIG_TOML`]: the same
//! prose with every assignment and header commented out, so it is inert and
//! later default changes still reach the user.
//!
//! [`DEFAULT_CONFIG_TOML`]: crate::DEFAULT_CONFIG_TOML

use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::DEFAULT_CONFIG_TOML;

/// Header replacing the embedded default's own preamble.
const SCAFFOLD_HEADER: &str = "\
# phux configuration.
#
# This is YOUR override file. phux ships its defaults compiled into the
# binary; everything below is the shipped default, commented out. While a
# line stays commented, phux uses the built-in default (so upgrades that
# change a default still reach you). Uncomment a line and edit it to
# override that one setting — anything you leave commented keeps tracking
# the default.
#
# Run `phux config show --default` to see the live annotated defaults, or
# `phux config show` to see your effective config after overrides.

";

/// Header for [`distro_reference_config`]; `{distro}` is the layer path.
const DISTRO_SCAFFOLD_HEADER: &str = "\
# phux configuration, scaffolded on top of a starter distribution.
#
# The `extends` line below layers the distro at
#   {distro}
# between phux's built-in defaults and this file (defaults <- distro <-
# you; see docs/CONFIG.md \"Layered configs\"). Every key you leave unset
# tracks the distro, and every key the distro leaves unset tracks the
# shipped default — so updates to either keep reaching you. Uncomment a
# line below and edit it to override that one setting.
#
# Run `phux config show` to see the effective merged config.

";

/// Outcome of a [`write_scaffold`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaffoldOutcome {
    /// The starter config was written to this path.
    Wrote(PathBuf),
    /// A file already existed and `force` was `false`.
    Skipped(PathBuf),
}

/// The commented starter config: [`SCAFFOLD_HEADER`] plus the projection.
#[must_use]
pub fn reference_config() -> String {
    let mut out = String::from(SCAFFOLD_HEADER);
    out.push_str(&commented_default_body());
    out
}

/// The distro-flavored starter: one active `extends` line pointing at the
/// (absolute) `distro`, then the same inert projection.
#[must_use]
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "`{distro}` is this scaffold's own template placeholder, not a std format arg"
)]
pub fn distro_reference_config(distro: &Path) -> String {
    let distro_display = distro.display().to_string();
    let mut out = DISTRO_SCAFFOLD_HEADER.replace("{distro}", &distro_display);
    out.push_str("extends = [");
    out.push_str(&toml_basic_string(&distro_display));
    out.push_str("]\n\n");
    out.push_str(&commented_default_body());
    out
}

/// Quote `s` as a TOML basic string (escaping `"`, `\`, and control
/// characters), so arbitrary filesystem paths survive the scaffold.
fn toml_basic_string(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// [`DEFAULT_CONFIG_TOML`]'s body from its first TOML line on, with every
/// line that is not blank or a comment commented out.
fn commented_default_body() -> String {
    let is_prose = |line: &str| {
        let trimmed = line.trim_start();
        trimmed.is_empty() || trimmed.starts_with('#')
    };
    let mut out = String::new();
    for line in DEFAULT_CONFIG_TOML
        .lines()
        .skip_while(|line| is_prose(line))
    {
        if !is_prose(line) {
            out.push_str("# ");
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Write a rendered scaffold to `path`, creating parent directories. An
/// existing file is left alone unless `force`.
///
/// # Errors
///
/// The underlying [`io::Error`] from creating the directory or writing.
pub fn write_scaffold(path: &Path, contents: &str, force: bool) -> io::Result<ScaffoldOutcome> {
    if path.exists() && !force {
        return Ok(ScaffoldOutcome::Skipped(path.to_path_buf()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(ScaffoldOutcome::Wrote(path.to_path_buf()))
}
