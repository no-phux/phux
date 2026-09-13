//! Starter-distribution resolution for `phux config init --distro`.
//!
//! A *distro* is an ordinary config layer (ADR-0039) curated as a
//! starting point — keybindings, a status lineup, a theme, a plugin set.
//! `phux config init --distro <spec>` scaffolds a user config whose
//! `extends` points at the distro layer, so the user's file stays a
//! sparse overlay and distro updates keep reaching them.
//!
//! `<spec>` is either a **path** (contains a separator or ends in
//! `.toml`; a directory means `<dir>/<dirname>.toml`) or a **bundled
//! name** looked up as `<dir>/<name>/<name>.toml` across the search
//! directories returned by [`search_dirs`]. `herdr` is kept as an alias
//! of `starter` so existing `--distro herdr` invocations still resolve
//! (and `distros/herdr/herdr.toml` remains as a path-level stub for
//! configs that baked the old absolute path):
//!
//! 1. `$PHUX_DISTROS_DIR` — explicit override (also the test hook).
//! 2. `$XDG_DATA_HOME/phux/distros` (or `~/.local/share/phux/distros`)
//!    — where an installer or the user places distro packages.
//! 3. The repo checkout's `distros/` directory, via the compile-time
//!    crate path — a dev-build convenience; on an installed binary the
//!    baked path simply fails the existence check and is skipped.
//!
//! Resolution canonicalizes the hit: the scaffolded `extends` entry must
//! be absolute because the user's config lives in a different directory.

use std::path::{Path, PathBuf};

/// Environment variable naming the preferred bundled-distro directory.
///
/// When set, `<dir>/<name>/<name>.toml` is checked before the XDG data
/// dir and the repo-checkout fallback.
pub const DISTROS_DIR_ENV: &str = "PHUX_DISTROS_DIR";

/// Error raised while resolving a `--distro` spec.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DistroError {
    /// The spec named a path (or a bundled lookup hit one) that could
    /// not be canonicalized — missing file, permission failure, ...
    #[error("distro layer {}: {source}", path.display())]
    Unreadable {
        /// The path that failed.
        path: PathBuf,
        /// The underlying failure.
        source: std::io::Error,
    },
    /// A bare name matched no bundled distro in any search directory.
    #[error(
        "unknown distro `{name}`; looked for {}",
        format_candidates(candidates)
    )]
    UnknownName {
        /// The bundled name that was looked up.
        name: String,
        /// Every candidate path that was checked, in search order.
        candidates: Vec<PathBuf>,
    },
}

fn format_candidates(candidates: &[PathBuf]) -> String {
    if candidates.is_empty() {
        return "(no distro search directories available)".to_owned();
    }
    candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolve a `--distro` spec to the absolute path of its layer file.
///
/// Path specs (a separator or a `.toml` suffix) resolve against the
/// current working directory; a directory path means
/// `<dir>/<dirname>.toml`. Bare names search [`search_dirs`] for
/// `<dir>/<name>/<name>.toml`, first hit wins.
///
/// # Errors
///
/// [`DistroError::Unreadable`] when the named (or matched) file cannot
/// be canonicalized; [`DistroError::UnknownName`] when a bare name
/// matches nothing, listing every path that was checked.
pub fn resolve_distro(spec: &str) -> Result<PathBuf, DistroError> {
    resolve_distro_in(spec, &search_dirs())
}

/// [`resolve_distro`] against an explicit search-directory list.
///
/// Split out so tests (and future embedders) can inject directories
/// instead of mutating process environment.
///
/// # Errors
///
/// See [`resolve_distro`].
pub fn resolve_distro_in(spec: &str, dirs: &[PathBuf]) -> Result<PathBuf, DistroError> {
    if spec_is_path(spec) {
        let path = Path::new(spec);
        let file = if path.is_dir() {
            path.file_name().map_or_else(
                || path.to_path_buf(),
                |dir_name| {
                    let mut name = dir_name.to_os_string();
                    name.push(".toml");
                    path.join(name)
                },
            )
        } else {
            path.to_path_buf()
        };
        return canonicalize(&file);
    }

    let mut candidates = Vec::new();
    for name in bundled_lookup_names(spec) {
        for dir in dirs {
            let candidate = dir.join(name).join(format!("{name}.toml"));
            if candidate.is_file() {
                return canonicalize(&candidate);
            }
            candidates.push(candidate);
        }
    }
    Err(DistroError::UnknownName {
        name: spec.to_owned(),
        candidates,
    })
}

/// Bundled names that still resolve after a distro was renamed.
///
/// The requested name is tried first so a user-supplied package of that
/// name in `$PHUX_DISTROS_DIR` keeps winning; the alias is the fallback
/// used by the repo checkout (`herdr` → `starter`).
const DISTRO_NAME_ALIASES: &[(&str, &str)] = &[("herdr", "starter")];

fn bundled_lookup_names(spec: &str) -> Vec<&str> {
    match DISTRO_NAME_ALIASES
        .iter()
        .copied()
        .find(|(from, _)| *from == spec)
    {
        Some((_, to)) if to != spec => vec![spec, to],
        _ => vec![spec],
    }
}

/// The bundled-name search directories, in precedence order. See the
/// module docs for the rationale behind each entry.
#[must_use]
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(env_dir) = std::env::var_os(DISTROS_DIR_ENV) {
        dirs.push(PathBuf::from(env_dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(Path::new(&xdg).join("phux").join("distros"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(
            Path::new(&home)
                .join(".local")
                .join("share")
                .join("phux")
                .join("distros"),
        );
    }
    // Dev-build convenience: the repo checkout's distros/ directory. The
    // path is baked at compile time; when the binary runs somewhere the
    // checkout does not exist, the existence check above skips it.
    dirs.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("distros"),
    );
    dirs
}

/// A spec containing a path separator or a `.toml` suffix is a path;
/// anything else is a bundled name. Mirrors the ADR-0039 `extends`
/// entry classification.
fn spec_is_path(spec: &str) -> bool {
    let has_toml_suffix = Path::new(spec)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    spec.contains('/') || spec.contains(std::path::MAIN_SEPARATOR) || has_toml_suffix
}

fn canonicalize(path: &Path) -> Result<PathBuf, DistroError> {
    path.canonicalize()
        .map_err(|source| DistroError::Unreadable {
            path: path.to_path_buf(),
            source,
        })
}
