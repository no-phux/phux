//! Distro discovery, resolution, and comment-preserving local selection.
//!
//! A distro is an ordinary config layer (ADR-0039) the scaffold `extends`.
//! `<spec>` is a path (a separator or `.toml`; a directory means
//! `<dir>/<dirname>.toml`) or a bundled name looked up as
//! `<dir>/<name>/<name>.toml` in `$PHUX_DISTROS_DIR`, the XDG data dir, then
//! the repo checkout's `distros/`. The hit is canonicalized, since the
//! user's config lives elsewhere.

use std::path::{Path, PathBuf};

mod switching;
pub use switching::{current_distro, switch_distro};

/// Environment variable naming the preferred bundled-distro directory.
pub const DISTROS_DIR_ENV: &str = "PHUX_DISTROS_DIR";

/// Error raised while resolving a `--distro` spec.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DistroError {
    /// A bare name is not a safe package identifier.
    #[error(
        "invalid distro name `{0}`; use ASCII letters, digits, `_` or `-`, or an explicit path"
    )]
    InvalidName(String),
    /// The resolved path could not be canonicalized.
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
/// `checkout_distros` is a source checkout's `distros/` directory, searched
/// last; see [`search_dirs`].
///
/// # Errors
///
/// [`DistroError::Unreadable`] when the file cannot be canonicalized;
/// [`DistroError::UnknownName`] when a bare name matches nothing.
pub fn resolve_distro(spec: &str, checkout_distros: Option<&Path>) -> Result<PathBuf, DistroError> {
    resolve_distro_in(spec, &search_dirs(checkout_distros))
}

/// [`resolve_distro`] against an explicit search-directory list.
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

    validate_name(spec)?;
    let mut candidates = Vec::new();
    for name in bundled_lookup_names(spec) {
        for dir in dirs {
            let candidate = dir.join(name).join(format!("{name}.toml"));
            if candidate_exists(&candidate)? {
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

/// One discoverable distro. The first search-directory hit wins per name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Distro {
    /// Safe bundled name accepted by [`resolve_distro_in`].
    pub name: String,
    /// Absolute layer path (the layer is validated when selected).
    pub path: PathBuf,
}

/// Discover bundled distros, sorted by name and deduplicated in search order.
///
/// # Errors
/// Unreadable search directories or layer paths; missing directories are ignored.
pub fn list_distros_in(dirs: &[PathBuf]) -> Result<Vec<Distro>, DistroError> {
    let mut found = std::collections::BTreeMap::new();
    for dir in dirs {
        for name in package_names(dir)? {
            if found.contains_key(&name) {
                continue;
            }
            let path = resolve_distro_in(&name, std::slice::from_ref(dir))?;
            found.insert(name.clone(), Distro { name, path });
        }
    }
    Ok(found.into_values().collect())
}

fn package_names(dir: &Path) -> Result<Vec<String>, DistroError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DistroError::Unreadable {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| DistroError::Unreadable {
            path: dir.to_path_buf(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if validate_name(&name).is_ok()
            && candidate_exists(&entry.path().join(format!("{name}.toml")))?
        {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn validate_name(name: &str) -> Result<(), DistroError> {
    let safe = !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'));
    if safe {
        Ok(())
    } else {
        Err(DistroError::InvalidName(name.to_owned()))
    }
}

fn candidate_exists(path: &Path) -> Result<bool, DistroError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(source) => Err(DistroError::Unreadable {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Renamed bundled distros; the requested name is still tried first.
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

/// The bundled-name search directories, in precedence order.
///
/// `checkout_distros`, when given, is appended last: the binary passes its
/// own source checkout's `distros/` as a dev-build convenience. It is a
/// parameter rather than a path baked in here because a compile-time
/// checkout path keys this crate, and every crate above it, to one checkout
/// in a content-addressed build cache.
#[must_use]
pub fn search_dirs(checkout_distros: Option<&Path>) -> Vec<PathBuf> {
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
    dirs.extend(checkout_distros.map(Path::to_path_buf));
    dirs
}

/// A path separator or `.toml` suffix makes a spec a path.
fn spec_is_path(spec: &str) -> bool {
    let has_toml_suffix = Path::new(spec)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    spec.contains('/') || spec.contains(std::path::MAIN_SEPARATOR) || has_toml_suffix
}

fn canonicalize(path: &Path) -> Result<PathBuf, DistroError> {
    path.canonicalize()
        .and_then(|resolved| {
            if resolved.is_file() {
                Ok(resolved)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "not a layer file",
                ))
            }
        })
        .map_err(|source| DistroError::Unreadable {
            path: path.to_path_buf(),
            source,
        })
}
