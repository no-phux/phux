//! Layered config resolution and merge (ADR-0039).
//!
//! A config file may declare a top-level `extends = ["path-or-name"]`
//! array. The effective config is an ordered stack — embedded
//! `default.toml` <- extended layers (depth-first, in listed order) <-
//! the declaring file — folded with the same recursive table merge the
//! two-layer scheme used, plus one addition: a key ending in `-append`
//! whose value is an array appends to (rather than replaces) the array
//! under the base key.
//!
//! Resolution is bounded ([`MAX_EXTENDS_DEPTH`]) and acyclic; a layer
//! reachable via two branches (diamond) is merged once, at its first
//! position. Every failure names the offending layer file.
//!
//! The merge records **provenance** as it folds: which layer set each
//! effective leaf key, and — for arrays — which layer contributed each
//! element. [`merged_config_with_provenance`] returns the merged table
//! together with a [`ConfigProvenance`]; `phux config show --layers`
//! renders it.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::{ConfigError, byte_offset_to_line_col};

/// Maximum `extends` nesting below the root config file.
///
/// The root file's layers sit at depth 1; a file at depth
/// `MAX_EXTENDS_DEPTH` may not declare `extends`. Deep enough for
/// user <- distro <- distro-base stacks with room to spare; small
/// enough that a runaway include graph fails fast.
pub const MAX_EXTENDS_DEPTH: usize = 4;

const EXTENDS_KEY: &str = "extends";
const APPEND_SUFFIX: &str = "-append";

/// Display path used for the embedded defaults layer in errors.
const DEFAULTS_DISPLAY_PATH: &str = "<embedded default.toml>";

/// One layer of the resolved config stack, in merge order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerSource {
    /// The `default.toml` embedded in the phux binary — always the
    /// first (lowest-precedence) layer.
    Defaults,
    /// A layer file pulled in via `extends` (ADR-0039).
    Extended(PathBuf),
    /// The root config file (the user's `config.toml`) — always the
    /// last (highest-precedence) layer.
    User(PathBuf),
}

impl LayerSource {
    /// The on-disk path of this layer, if it has one (the embedded
    /// defaults do not).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Defaults => None,
            Self::Extended(p) | Self::User(p) => Some(p),
        }
    }
}

/// Provenance of one effective leaf key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyOrigin {
    /// Index into [`ConfigProvenance::layers`] of the layer that last
    /// set — or, for arrays, last appended to — this key.
    pub layer: usize,
    /// For arrays: the contributing layer index of each element, in
    /// element order (`-append` elements carry the appending layer;
    /// a plain assignment attributes every element to the assigning
    /// layer). `None` for non-array leaves.
    pub elements: Option<Vec<usize>>,
}

/// Which layer set each effective config key (ADR-0039 attribution).
///
/// Produced by [`merged_config_with_provenance`]. Keys are dotted
/// paths to the *leaf* values of the merged table (tables themselves
/// carry no entry; array elements are attributed via
/// [`KeyOrigin::elements`]). Path segments that are not bare TOML keys
/// are double-quoted, so entries read like TOML addresses, e.g.
/// `keybindings.prefix-table."%"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProvenance {
    /// The resolved layer stack in merge order: `Defaults` first, the
    /// root `User` file last, `Extended` layers in between.
    pub layers: Vec<LayerSource>,
    /// Dotted leaf path -> origin, sorted by path.
    pub keys: BTreeMap<String, KeyOrigin>,
}

/// Top-level array keys whose elements carry a `manifest` path.
///
/// Plugin loaders resolve a relative `[[plugins]]` manifest against the
/// *user config file's* directory (`crate::plugin::resolve_manifest_path`).
/// A shared layer — a distro, a team baseline — lives somewhere else
/// entirely, so a relative manifest it declares would dangle once merged.
/// Layer resolution therefore rewrites those paths to absolute against
/// the layer's own directory before the merge erases provenance.
const MANIFEST_ARRAY_KEYS: [&str; 2] = ["plugins", "plugins-append"];

/// Parse `input` as a plain TOML table, mapping errors to
/// [`ConfigError::Parse`] with `line:col` pointing into `input`.
fn parse_table(input: &str, path: &Path) -> Result<toml::Table, ConfigError> {
    toml::from_str(input).map_err(|e| {
        let (line, col) = e
            .span()
            .map_or((1, 1), |r| byte_offset_to_line_col(input, r.start));
        ConfigError::Parse {
            path: path.to_path_buf(),
            line,
            col,
            message: e.message().to_owned(),
        }
    })
}

/// Merge the full layer stack, returning table plus provenance.
///
/// The stack is the embedded defaults, any layers named via `extends`
/// (ADR-0039), then `user_input`; the [`ConfigProvenance`] is recorded
/// during the fold.
///
/// The table half is exactly what [`crate::merged_config_table`]
/// returns (that function delegates here); the provenance half backs
/// `phux config show --layers`.
///
/// `path` is used for error reporting on `user_input` and as the base
/// directory for relative `extends` entries; layer files are read from
/// disk. When `user_input` declares no `extends`, no I/O occurs.
///
/// # Errors
///
/// Returns [`ConfigError::Parse`] if the embedded defaults,
/// `user_input`, or a layer file are not valid TOML;
/// [`ConfigError::LayerRead`] / [`ConfigError::LayerCycle`] /
/// [`ConfigError::Layer`] for layer-resolution and `-append` failures,
/// each naming the offending file.
pub fn merged_config_with_provenance(
    user_input: &str,
    path: &Path,
) -> Result<(toml::Table, ConfigProvenance), ConfigError> {
    let defaults_path = Path::new(DEFAULTS_DISPLAY_PATH);
    let default_table = parse_table(crate::DEFAULT_CONFIG_TOML, defaults_path)?;
    let stack = resolve_user_stack(user_input, path)?;

    let mut layers = vec![LayerSource::Defaults];
    let mut recorded = BTreeMap::new();
    // Fold the defaults from an empty base so their keys are recorded
    // like any other layer's; the result is the defaults table itself.
    let mut merged = merge_layer(
        toml::Table::new(),
        default_table,
        defaults_path,
        "",
        &mut Recorder {
            layer: 0,
            keys: &mut recorded,
        },
    )?;

    // `resolve_user_stack` always ends with the root (user) file.
    let last = stack.len().saturating_sub(1);
    for (i, (layer_path, table)) in stack.into_iter().enumerate() {
        let layer_idx = layers.len();
        layers.push(if i == last {
            LayerSource::User(layer_path.clone())
        } else {
            LayerSource::Extended(layer_path.clone())
        });
        merged = merge_layer(
            merged,
            table,
            &layer_path,
            "",
            &mut Recorder {
                layer: layer_idx,
                keys: &mut recorded,
            },
        )?;
    }

    let mut keys = BTreeMap::new();
    finalize_keys(&merged, &recorded, "", &mut keys);
    Ok((merged, ConfigProvenance { layers, keys }))
}

/// Resolve the ordered layer stack rooted at `user_input` / `path`.
///
/// Returns `(layer path, table)` pairs in merge order: extended layers
/// first (depth-first, in listed order), the root file last. Each
/// table has its `extends` key consumed.
fn resolve_user_stack(
    user_input: &str,
    path: &Path,
) -> Result<Vec<(PathBuf, toml::Table)>, ConfigError> {
    let root = parse_table(user_input, path)?;
    let mut out = Vec::new();
    // The root is on the chain from the start, so a layer that extends
    // the user's own config file is reported as a cycle.
    let mut visiting = vec![canonical(path)];
    let mut seen = HashSet::new();
    push_layer(path, root, 0, &mut visiting, &mut seen, &mut out)?;
    Ok(out)
}

/// Depth-first post-order walk: resolve `table`'s `extends` chain into
/// `out`, then push `table` itself.
fn push_layer(
    path: &Path,
    mut table: toml::Table,
    depth: usize,
    visiting: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<(PathBuf, toml::Table)>,
) -> Result<(), ConfigError> {
    if let Some(value) = table.remove(EXTENDS_KEY) {
        if depth >= MAX_EXTENDS_DEPTH {
            return Err(ConfigError::Layer {
                path: path.to_path_buf(),
                message: format!(
                    "`extends` nesting exceeds the maximum depth of {MAX_EXTENDS_DEPTH}"
                ),
            });
        }
        for entry in extends_entries(value, path)? {
            let layer_path = resolve_entry(&entry, path);
            let layer_canon = canonical(&layer_path);
            if visiting.contains(&layer_canon) {
                return Err(ConfigError::LayerCycle {
                    layer: layer_path,
                    referenced_from: path.to_path_buf(),
                });
            }
            if !seen.insert(layer_canon.clone()) {
                // Diamond: already merged via another branch. First
                // position wins (ADR-0039).
                continue;
            }
            let contents =
                std::fs::read_to_string(&layer_path).map_err(|source| ConfigError::LayerRead {
                    layer: layer_path.clone(),
                    referenced_from: path.to_path_buf(),
                    source,
                })?;
            let layer_table = parse_table(&contents, &layer_path)?;
            visiting.push(layer_canon);
            push_layer(&layer_path, layer_table, depth + 1, visiting, seen, out)?;
            visiting.pop();
        }
    }
    if depth > 0 {
        // Only *extended* layers are rewritten: the root file's relative
        // manifests already resolve against its own directory by the
        // documented `[[plugins]]` contract, and leaving them untouched
        // keeps `phux config show` output identical to what the user wrote.
        let layer_dir = path.parent().unwrap_or_else(|| Path::new(""));
        absolutize_plugin_manifests(&mut table, layer_dir);
    }
    out.push((path.to_path_buf(), table));
    Ok(())
}

/// Rewrite relative `manifest` paths in [`MANIFEST_ARRAY_KEYS`] arrays to
/// absolute paths under `layer_dir`, so a shared layer's plugin wiring
/// keeps working no matter where the user's config file lives.
fn absolutize_plugin_manifests(table: &mut toml::Table, layer_dir: &Path) {
    for key in MANIFEST_ARRAY_KEYS {
        let Some(toml::Value::Array(entries)) = table.get_mut(key) else {
            continue;
        };
        for entry in entries {
            let Some(toml::Value::String(manifest)) = entry
                .as_table_mut()
                .and_then(|entry| entry.get_mut("manifest"))
            else {
                continue;
            };
            if !Path::new(manifest.as_str()).is_absolute() {
                *manifest = lexical_normalize(&layer_dir.join(manifest.as_str()))
                    .display()
                    .to_string();
            }
        }
    }
}

/// Fold `.` and `..` components lexically (no filesystem access, no
/// symlink resolution) so a distro layer's `../../plugins/...` manifest
/// reads cleanly in `phux config show` output and error messages.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // The parent of the root is the root.
                Some(Component::RootDir) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Validate the `extends` value: an array of strings.
fn extends_entries(value: toml::Value, path: &Path) -> Result<Vec<String>, ConfigError> {
    let err = || ConfigError::Layer {
        path: path.to_path_buf(),
        message: "`extends` must be an array of strings (layer paths or names)".to_owned(),
    };
    let toml::Value::Array(items) = value else {
        return Err(err());
    };
    items
        .into_iter()
        .map(|item| match item {
            toml::Value::String(s) => Ok(s),
            _ => Err(err()),
        })
        .collect()
}

/// Map one `extends` entry to a layer path (ADR-0039): absolute paths
/// pass through; anything with a path separator or a `.toml` suffix is
/// relative to the declaring file's directory; a bare name `n` means
/// `layers/n.toml` beside the declaring file.
fn resolve_entry(entry: &str, declaring: &Path) -> PathBuf {
    let candidate = Path::new(entry);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    let base = declaring.parent().unwrap_or_else(|| Path::new(""));
    let has_toml_suffix = candidate
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    if entry.contains(std::path::MAIN_SEPARATOR) || entry.contains('/') || has_toml_suffix {
        base.join(candidate)
    } else {
        base.join("layers").join(format!("{entry}.toml"))
    }
}

/// Canonical identity for cycle / diamond detection. Falls back to the
/// lexical path when canonicalization fails (e.g. the root path names
/// no real file, as in pure-string parses); the read step reports the
/// real error for missing layers.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Provenance recorder threaded through one layer's merge: the layer's
/// stack index plus the shared path -> origin map.
struct Recorder<'a> {
    layer: usize,
    keys: &'a mut BTreeMap<String, KeyOrigin>,
}

impl Recorder<'_> {
    /// A plain assignment set `path` to `value` (replacing whatever a
    /// lower layer put there).
    fn record_set(&mut self, path: &str, value: &toml::Value) {
        let elements = match value {
            toml::Value::Array(items) => Some(vec![self.layer; items.len()]),
            _ => None,
        };
        self.keys.insert(
            path.to_owned(),
            KeyOrigin {
                layer: self.layer,
                elements,
            },
        );
    }

    /// An `-append` directive added `added` elements to the array at
    /// `path` (creating it when absent).
    fn record_append(&mut self, path: &str, added: usize) {
        match self.keys.get_mut(path) {
            Some(origin) if origin.elements.is_some() => {
                origin.layer = self.layer;
                if let Some(elements) = origin.elements.as_mut() {
                    elements.extend(std::iter::repeat_n(self.layer, added));
                }
            }
            // No lower layer recorded an array here (or the recorded
            // shape was not an array, which the merge itself rejects):
            // the append created the array, so it owns every element.
            _ => {
                self.keys.insert(
                    path.to_owned(),
                    KeyOrigin {
                        layer: self.layer,
                        elements: Some(vec![self.layer; added]),
                    },
                );
            }
        }
    }
}

/// Dotted-path segment for `key` under `prefix`: bare TOML keys join
/// with `.`; anything else is double-quoted so the path stays a valid
/// TOML address.
///
/// `pub(crate)` because [`crate::check`]'s semantic pass builds paths
/// for keybinding findings and they must spell keys exactly the way
/// provenance recorded them, or layer attribution silently misses.
#[allow(
    clippy::redundant_pub_crate,
    reason = "`pub` here would trip `unreachable_pub`: the module is \
              private and this fn is not re-exported"
)]
pub(crate) fn child_path(prefix: &str, key: &str) -> String {
    let is_bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let segment = if is_bare {
        key.to_owned()
    } else {
        format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
    };
    if prefix.is_empty() {
        segment
    } else {
        format!("{prefix}.{segment}")
    }
}

/// Recursively merge `overlay` (from the layer file at `layer`) into
/// `base`, recording provenance into `recorder`.
///
/// Tables merge per key; any other value type — including arrays —
/// replaces wholesale. A key `x-append` holding an array appends its
/// elements to `base`'s `x` (creating it when absent) instead of
/// replacing. Misuse — appending to a non-array, a non-array append
/// value, or `x` and `x-append` in the same overlay table — is an
/// error naming `layer`.
fn merge_layer(
    mut base: toml::Table,
    overlay: toml::Table,
    layer: &Path,
    prefix: &str,
    recorder: &mut Recorder<'_>,
) -> Result<toml::Table, ConfigError> {
    let layer_err = |message: String| ConfigError::Layer {
        path: layer.to_path_buf(),
        message,
    };

    // Split plain keys from `-append` directives; plain keys apply
    // first so append order is deterministic regardless of key order.
    let mut appends: Vec<(String, toml::Value)> = Vec::new();
    let mut plain = toml::Table::new();
    for (key, value) in overlay {
        match key.strip_suffix(APPEND_SUFFIX) {
            Some(target) if !target.is_empty() => appends.push((target.to_owned(), value)),
            _ => {
                plain.insert(key, value);
            }
        }
    }
    for (target, _) in &appends {
        if plain.contains_key(target) {
            return Err(layer_err(format!(
                "both `{target}` and `{target}{APPEND_SUFFIX}` are set in the same layer; \
                 use one (`{target}` replaces, `{target}{APPEND_SUFFIX}` appends)"
            )));
        }
    }

    for (key, value) in plain {
        let path = child_path(prefix, &key);
        match (base.remove(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => {
                base.insert(
                    key,
                    toml::Value::Table(merge_layer(b, o, layer, &path, recorder)?),
                );
            }
            (_, toml::Value::Table(o)) => {
                // No base table to merge into, but the overlay table
                // may still carry nested `-append` directives (e.g.
                // `[[hooks.<name>-append]]` when the base defines no
                // hooks at all); normalize them against an empty base
                // so directive keys never leak into the final table.
                base.insert(
                    key,
                    toml::Value::Table(merge_layer(toml::Table::new(), o, layer, &path, recorder)?),
                );
            }
            (_, v) => {
                recorder.record_set(&path, &v);
                base.insert(key, v);
            }
        }
    }

    for (target, value) in appends {
        let path = child_path(prefix, &target);
        let toml::Value::Array(mut additions) = value else {
            return Err(layer_err(format!(
                "`{target}{APPEND_SUFFIX}` must be an array (it appends to the array `{target}`)"
            )));
        };
        let added = additions.len();
        match base.remove(&target) {
            None => {
                base.insert(target, toml::Value::Array(additions));
            }
            Some(toml::Value::Array(mut existing)) => {
                existing.append(&mut additions);
                base.insert(target, toml::Value::Array(existing));
            }
            Some(_) => {
                return Err(layer_err(format!(
                    "`{target}{APPEND_SUFFIX}` targets `{target}`, which is not an array"
                )));
            }
        }
        recorder.record_append(&path, added);
    }

    Ok(base)
}

/// Project the recorded origins onto the *final* merged table: walk
/// its leaves and keep exactly one entry per leaf path. This drops
/// entries left stale by shape changes across layers (a scalar later
/// replaced by a table leaves its old leaf entry behind; the walk
/// never visits it).
fn finalize_keys(
    table: &toml::Table,
    recorded: &BTreeMap<String, KeyOrigin>,
    prefix: &str,
    out: &mut BTreeMap<String, KeyOrigin>,
) {
    for (key, value) in table {
        let path = child_path(prefix, key);
        match value {
            toml::Value::Table(t) => finalize_keys(t, recorded, &path, out),
            leaf => {
                // Every leaf was inserted through the recorder, so the
                // lookup succeeds; the fallback (attribute to the
                // defaults layer) is purely defensive.
                let mut origin = recorded.get(&path).cloned().unwrap_or(KeyOrigin {
                    layer: 0,
                    elements: None,
                });
                if let toml::Value::Array(items) = leaf {
                    let matches = origin
                        .elements
                        .as_ref()
                        .is_some_and(|e| e.len() == items.len());
                    if !matches {
                        origin.elements = Some(vec![origin.layer; items.len()]);
                    }
                } else {
                    origin.elements = None;
                }
                out.insert(path, origin);
            }
        }
    }
}
