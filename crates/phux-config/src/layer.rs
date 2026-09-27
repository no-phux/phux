//! Layered config resolution and merge (ADR-0039).
//!
//! A file may declare `extends = ["path-or-name"]`. The effective config is
//! embedded `default.toml` <- extended layers (depth-first, listed order) <-
//! the declaring file, merged recursively; a key `x-append` holding an array
//! appends to `x` instead of replacing it. Resolution is depth-bounded and
//! acyclic, a diamond layer merges once at its first position, and every
//! failure names the offending file. The fold records which layer set each
//! leaf (and each array element) for `phux config show --layers`.

use std::collections::{BTreeMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::ConfigError;

/// Maximum `extends` nesting below the root config file (whose layers sit at
/// depth 1).
pub const MAX_EXTENDS_DEPTH: usize = 4;

const EXTENDS_KEY: &str = "extends";
const APPEND_SUFFIX: &str = "-append";

const DEFAULTS_DISPLAY_PATH: &str = "<embedded default.toml>";

/// One layer of the resolved config stack, in merge order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerSource {
    /// The embedded `default.toml`; always first.
    Defaults,
    /// A layer file pulled in via `extends`.
    Extended(PathBuf),
    /// The root config file; always last.
    User(PathBuf),
}

impl LayerSource {
    /// The on-disk path of this layer (the embedded defaults have none).
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
    /// Index into [`ConfigProvenance::layers`] of the layer that last set
    /// (or appended to) this key.
    pub layer: usize,
    /// For arrays, the contributing layer of each element.
    pub elements: Option<Vec<usize>>,
}

/// Which layer set each effective leaf key. Keys are dotted TOML addresses,
/// non-bare segments double-quoted (`keybindings.prefix-table."%"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProvenance {
    /// The resolved layer stack in merge order.
    pub layers: Vec<LayerSource>,
    /// Dotted leaf path -> origin, sorted by path.
    pub keys: BTreeMap<String, KeyOrigin>,
}

/// Top-level arrays whose elements carry a `manifest` path. A relative
/// manifest in a shared layer is made absolute against that layer's
/// directory, since `[[plugins]]` otherwise resolve against the user's file.
const MANIFEST_ARRAY_KEYS: [&str; 2] = ["plugins", "plugins-append"];

/// Parse `input` as a plain TOML table.
#[allow(
    clippy::redundant_pub_crate,
    reason = "private module helper; pub would trip unreachable_pub"
)]
pub(crate) fn parse_table(input: &str, path: &Path) -> Result<toml::Table, ConfigError> {
    toml::from_str(input).map_err(|e| ConfigError::parse(path, input, e.span(), e.message()))
}

/// Merge the full layer stack (defaults, `extends` layers, `user_input`) and
/// record provenance. `path` anchors relative `extends` entries; with no
/// `extends`, no I/O occurs.
///
/// # Errors
///
/// [`ConfigError::Parse`] for invalid TOML in any layer;
/// [`ConfigError::LayerRead`] / [`ConfigError::LayerCycle`] /
/// [`ConfigError::Layer`] for resolution and `-append` failures.
pub fn merged_config_with_provenance(
    user_input: &str,
    path: &Path,
) -> Result<(toml::Table, ConfigProvenance), ConfigError> {
    merged_with_budget(user_input, path, None)
}

#[allow(
    clippy::redundant_pub_crate,
    reason = "private module helper; pub would trip unreachable_pub"
)]
pub(crate) fn merged_with_budget(
    user_input: &str,
    path: &Path,
    max_read_bytes: Option<usize>,
) -> Result<(toml::Table, ConfigProvenance), ConfigError> {
    let defaults_path = Path::new(DEFAULTS_DISPLAY_PATH);
    let default_table = parse_table(crate::DEFAULT_CONFIG_TOML, defaults_path)?;
    let stack = resolve_user_stack(user_input, path, max_read_bytes)?;

    let mut layers = vec![LayerSource::Defaults];
    let mut recorded = BTreeMap::new();
    // Fold the defaults from an empty base so their keys are recorded too.
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

/// `(layer path, table)` pairs in merge order, root file last, each with its
/// `extends` key consumed.
fn resolve_user_stack(
    user_input: &str,
    path: &Path,
    max_read_bytes: Option<usize>,
) -> Result<Vec<(PathBuf, toml::Table)>, ConfigError> {
    let mut budget = ReadBudget(max_read_bytes);
    budget.consume(user_input.len(), path, path)?;
    let root = parse_table(user_input, path)?;
    // The root is on the chain, so extending the user's own file is a cycle.
    let mut resolver = LayerResolver {
        visiting: vec![canonical(path)],
        seen: HashSet::new(),
        out: Vec::new(),
        budget,
    };
    resolver.push(path, root, 0)?;
    Ok(resolver.out)
}

/// Depth-first post-order walk: resolve `table`'s `extends` chain into
/// `out`, then push `table` itself.
struct LayerResolver {
    visiting: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
    out: Vec<(PathBuf, toml::Table)>,
    budget: ReadBudget,
}

impl LayerResolver {
    fn push(
        &mut self,
        path: &Path,
        mut table: toml::Table,
        depth: usize,
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
                self.extend(path, &entry, depth + 1)?;
            }
        }
        if depth > 0 {
            let layer_dir = path.parent().unwrap_or_else(|| Path::new(""));
            absolutize_plugin_manifests(&mut table, layer_dir);
        }
        self.out.push((path.to_path_buf(), table));
        Ok(())
    }

    fn extend(&mut self, parent: &Path, entry: &str, depth: usize) -> Result<(), ConfigError> {
        let path = resolve_entry(entry, parent);
        let canonical = canonical(&path);
        if self.visiting.contains(&canonical) {
            return Err(ConfigError::LayerCycle {
                layer: path,
                referenced_from: parent.to_path_buf(),
            });
        }
        // Diamond: first position wins and consumes the file budget only once.
        if !self.seen.insert(canonical.clone()) {
            return Ok(());
        }
        let contents = self.budget.read(&path, parent)?;
        let table = parse_table(&contents, &path)?;
        self.visiting.push(canonical);
        self.push(&path, table, depth)?;
        self.visiting.pop();
        Ok(())
    }
}

/// Aggregate byte budget over the root input and each unique external layer.
struct ReadBudget(Option<usize>);

impl ReadBudget {
    fn consume(&mut self, bytes: usize, path: &Path, parent: &Path) -> Result<(), ConfigError> {
        let Some(remaining) = self.0 else {
            return Ok(());
        };
        self.0 = Some(remaining.checked_sub(bytes).ok_or_else(|| {
            layer_read_error(
                path,
                parent,
                std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "aggregate configuration byte budget exceeded",
                ),
            )
        })?);
        Ok(())
    }

    fn read(&mut self, path: &Path, parent: &Path) -> Result<String, ConfigError> {
        let Some(remaining) = self.0 else {
            return std::fs::read_to_string(path)
                .map_err(|err| layer_read_error(path, parent, err));
        };
        let mut contents = Vec::new();
        let file = std::fs::File::open(path).map_err(|err| layer_read_error(path, parent, err))?;
        file.take(remaining.saturating_add(1) as u64)
            .read_to_end(&mut contents)
            .map_err(|err| layer_read_error(path, parent, err))?;
        self.consume(contents.len(), path, parent)?;
        String::from_utf8(contents).map_err(|err| {
            layer_read_error(
                path,
                parent,
                std::io::Error::new(std::io::ErrorKind::InvalidData, err),
            )
        })
    }
}

fn layer_read_error(path: &Path, parent: &Path, source: std::io::Error) -> ConfigError {
    ConfigError::LayerRead {
        layer: path.to_path_buf(),
        referenced_from: parent.to_path_buf(),
        source,
    }
}

/// Make relative `manifest` paths in [`MANIFEST_ARRAY_KEYS`] absolute under
/// `layer_dir`.
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

/// Fold `.` and `..` components lexically, without touching the filesystem.
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

/// Map one `extends` entry to a layer path: absolute passes through, a path
/// or `.toml` name is relative to the declaring file, and a bare name `n`
/// means `layers/n.toml` beside it.
fn resolve_entry(entry: &str, declaring: &Path) -> PathBuf {
    let candidate = Path::new(entry);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        let base = declaring.parent().unwrap_or_else(|| Path::new(""));
        let has_toml_suffix = candidate
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
        if entry.contains(std::path::MAIN_SEPARATOR) || entry.contains('/') || has_toml_suffix {
            base.join(candidate)
        } else {
            base.join("layers").join(format!("{entry}.toml"))
        }
    };
    rewrite_retired_distro_layer(&resolved)
}

/// `distros/herdr/herdr.toml` was renamed to `distros/starter/starter.toml`,
/// and configs baked the old absolute path: when the herdr layer is gone
/// but the starter sits beside it, load the starter.
fn rewrite_retired_distro_layer(path: &Path) -> PathBuf {
    if path.is_file() {
        return path.to_path_buf();
    }
    herdr_layer_to_starter(path)
        .filter(|starter| starter.is_file())
        .unwrap_or_else(|| path.to_path_buf())
}

/// Map `.../distros/herdr/herdr.toml` to `.../distros/starter/starter.toml`.
fn herdr_layer_to_starter(path: &Path) -> Option<PathBuf> {
    let herdr_dir = path.parent()?;
    let distros = herdr_dir.parent()?;
    let is_herdr = path.file_name()? == "herdr.toml"
        && herdr_dir.file_name()? == "herdr"
        && distros.file_name()? == "distros";
    is_herdr.then(|| distros.join("starter").join("starter.toml"))
}

/// Canonical identity for cycle / diamond detection, or the lexical path
/// when the file does not exist (the read reports that).
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Provenance recorder for one layer's merge.
struct Recorder<'a> {
    layer: usize,
    keys: &'a mut BTreeMap<String, KeyOrigin>,
}

impl Recorder<'_> {
    /// A plain assignment replaced whatever lower layers set at `path`.
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

    /// An `-append` added `added` elements to the array at `path`.
    fn record_append(&mut self, path: &str, added: usize) {
        match self.keys.get_mut(path) {
            Some(origin) if origin.elements.is_some() => {
                origin.layer = self.layer;
                if let Some(elements) = origin.elements.as_mut() {
                    elements.extend(std::iter::repeat_n(self.layer, added));
                }
            }
            // The append created the array, so it owns every element.
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

/// Dotted path for `key` under `prefix`, double-quoting non-bare keys. Shared
/// with [`crate::check`] so its findings match provenance paths exactly.
#[allow(
    clippy::redundant_pub_crate,
    reason = "private module helper; pub would trip unreachable_pub"
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

/// Recursively merge `overlay` (from `layer`) into `base`. Tables merge per
/// key; everything else, arrays included, replaces. `x-append` appends to
/// `x`; appending to a non-array, a non-array append, or `x` beside
/// `x-append` in one table is an error naming `layer`.
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

    // Plain keys apply first so append order is deterministic.
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
                // Merge against an empty base so nested `-append`
                // directives never leak into the final table.
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

/// Project the recorded origins onto the final table's leaves, dropping
/// entries left stale by shape changes across layers.
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

#[cfg(test)]
mod budget_tests {
    #[test]
    fn aggregate_budget_counts_unique_diamond_layers_and_preserves_unbounded_loading() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("config.toml");
        let input = "extends=['a.toml','b.toml']\n";
        let branch = "extends=['common.toml']\n";
        let common = "[[remote]]\nname='x'\nendpoint='ssh://x'\n";
        std::fs::write(dir.path().join("a.toml"), branch).expect("a");
        std::fs::write(dir.path().join("b.toml"), branch).expect("b");
        std::fs::write(dir.path().join("common.toml"), common).expect("common");
        let exact = input.len() + 2 * branch.len() + common.len();
        let bounded =
            crate::parse_with_defaults_bounded(input, &root, exact).expect("exact budget");
        let ordinary = crate::parse_with_defaults(input, &root).expect("ordinary loader");
        assert_eq!(bounded.remote, ordinary.remote);
        assert!(crate::parse_with_defaults_bounded(input, &root, exact - 1).is_err());
        std::fs::write(
            dir.path().join("common.toml"),
            format!("{common}#{}", "界".repeat(1000)),
        )
        .expect("large common");
        assert!(crate::parse_with_defaults_bounded(input, &root, exact).is_err());
        assert_eq!(
            crate::parse_with_defaults(input, &root)
                .expect("unbounded behavior unchanged")
                .remote,
            ordinary.remote
        );
    }
}

#[cfg(test)]
mod retired_distro_tests {
    use super::{herdr_layer_to_starter, rewrite_retired_distro_layer};
    use std::path::Path;

    #[test]
    fn a_missing_herdr_layer_loads_the_starter_beside_it() {
        assert_eq!(
            herdr_layer_to_starter(Path::new("/tmp/not-a-distro/herdr.toml")),
            None
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let distros = dir.path().join("distros");
        std::fs::create_dir_all(distros.join("starter")).expect("starter dir");
        std::fs::create_dir_all(distros.join("herdr")).expect("herdr dir");
        let starter = distros.join("starter").join("starter.toml");
        std::fs::write(&starter, "defaults.history-limit = 12345\n").expect("starter.toml");
        let herdr = distros.join("herdr").join("herdr.toml");

        let user = dir.path().join("config.toml");
        let input = format!("extends = [\"{}\"]\n", herdr.display());
        let cfg = crate::parse_with_defaults(&input, &user).expect("retired path still loads");
        assert_eq!(cfg.defaults.history_limit, 12345);

        // A still-present herdr stub wins unchanged.
        std::fs::write(&herdr, "defaults.history-limit = 7\n").expect("stub");
        assert_eq!(rewrite_retired_distro_layer(&herdr), herdr);
    }
}
