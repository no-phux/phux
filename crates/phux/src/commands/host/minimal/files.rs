//! The files `phux host minimal` owns: the Minimal loadout and the
//! `default_loadouts` entry in Minimal's config, the record of which box
//! joined as which satellite, and the cached Linux phux boxes get.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Table, value};

/// Minimal's user config directory, `<config>/minimal`: `$XDG_CONFIG_HOME`
/// when it is absolute, else `~/.config`, on every platform (Minimal's
/// `paths::minimal_config_dir`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MinimalDirs {
    root: PathBuf,
}

impl MinimalDirs {
    pub(super) fn from_env() -> Result<Self, String> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or("neither XDG_CONFIG_HOME nor HOME is set")?;
        Ok(Self::at(base.join("minimal")))
    }

    pub(super) const fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// The directory layout (`<name>/loadout.toml`), so the loadout is one
    /// tree `remove` can delete.
    pub(super) fn loadout_file(&self, name: &str) -> PathBuf {
        self.root.join("loadouts").join(name).join("loadout.toml")
    }

    fn flat_loadout_file(&self, name: &str) -> PathBuf {
        self.root.join("loadouts").join(format!("{name}.toml"))
    }

    /// Write loadout `name`, patching `binary` into each box. Minimal refuses
    /// a name defined in both layouts, so a hand-written `<name>.toml` is
    /// refused here rather than breaking every activation.
    pub(super) fn write_loadout(&self, name: &str, binary: &Path) -> Result<(), String> {
        let flat = self.flat_loadout_file(name);
        if flat.exists() {
            return Err(format!(
                "{} already defines a `{name}` loadout; move it aside so this one can be written",
                flat.display()
            ));
        }
        let path = self.loadout_file(name);
        write_file(&path, &render_loadout(binary))
    }

    /// Add `name` to `[loadouts].default_loadouts`, keeping the rest of the
    /// file; `false` when it was already there.
    pub(super) fn add_default_loadout(&self, name: &str) -> Result<bool, String> {
        let path = self.config_file();
        let mut doc = read_document(&path)?;
        let defaults = default_loadouts(&mut doc, &path)?;
        if defaults.iter().any(|entry| entry.as_str() == Some(name)) {
            return Ok(false);
        }
        defaults.push(name);
        write_file(&path, &doc.to_string())?;
        Ok(true)
    }

    /// Remove loadout `name` and its `default_loadouts` entry; `false` when
    /// neither was there.
    pub(super) fn remove_loadout(&self, name: &str) -> Result<bool, String> {
        let mut removed = false;
        let dir = self.root.join("loadouts").join(name);
        if dir.join("loadout.toml").exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|err| format!("could not remove {}: {err}", dir.display()))?;
            removed = true;
        }
        let path = self.config_file();
        if path.exists() {
            let mut doc = read_document(&path)?;
            let defaults = default_loadouts(&mut doc, &path)?;
            let before = defaults.len();
            defaults.retain(|entry| entry.as_str() != Some(name));
            if defaults.len() != before {
                write_file(&path, &doc.to_string())?;
                removed = true;
            }
        }
        Ok(removed)
    }
}

/// The loadout: the Linux phux at `binary` lands on the box's `PATH`
/// (`~/.local/bin` is on it), and activation starts its server so the hub's
/// join finds one running. A box whose server cannot start still activates.
pub(super) fn render_loadout(binary: &Path) -> String {
    let source = value(binary.display().to_string());
    format!(
        "# Written by `phux host minimal setup`, which rewrites it; remove it with\n\
         # `phux host minimal remove`.\n\
         description = \"phux: this box joins your phux hub as a satellite\"\n\
         patches = [\n    {{ dest = \".local/bin/phux\", source = {} }},\n]\n\n\
         [[lifecycle_hooks]]\n\
         description = \"start phux so the hub can adopt this box\"\n\
         on_activate = {{ type = \"inline\", value = \"phux server --ensure >/dev/null 2>&1 || true\" }}\n",
        source.to_string().trim()
    )
}

fn read_document(path: &Path) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map_err(|err| format!("{} is not valid TOML: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(err) => Err(format!("could not read {}: {err}", path.display())),
    }
}

/// `[loadouts].default_loadouts`, created empty when absent.
fn default_loadouts<'doc>(
    doc: &'doc mut DocumentMut,
    path: &Path,
) -> Result<&'doc mut Array, String> {
    let shape = || {
        format!(
            "{}: `[loadouts].default_loadouts` must be an array",
            path.display()
        )
    };
    let table = doc
        .entry("loadouts")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(shape)?;
    table
        .entry("default_loadouts")
        .or_insert(value(Array::new()))
        .as_array_mut()
        .ok_or_else(shape)
}

fn write_file(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let staged = path.with_extension("tmp");
    std::fs::write(&staged, body)
        .and_then(|()| std::fs::rename(&staged, path))
        .map_err(|err| format!("could not write {}: {err}", path.display()))
}

/// Where this integration keeps its state: under the phux state directory,
/// so each profile has its own.
fn state_root() -> PathBuf {
    phux_server::telemetry::state_dir().join("minimal")
}

pub(super) fn registry_path() -> PathBuf {
    state_root().join("boxes.json")
}

/// Which Minimal session joined as which satellite, and which satellite
/// names this feature minted.
///
/// `{"schema_version":1,"boxes":{"<session id>":"<satellite>"},"minted":["<satellite>"]}`.
/// A file written before `minted` existed owns the names in `boxes`: those
/// joins are the record that this feature minted them. Every name in
/// `boxes` is owned, so a joined box can still be revoked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Joined {
    pub(super) boxes: BTreeMap<String, String>,
    pub(super) minted: BTreeSet<String>,
}

/// Which Minimal session joined as which satellite.
#[derive(Debug, Clone)]
pub(super) struct BoxRegistry {
    path: PathBuf,
}

impl BoxRegistry {
    pub(super) const fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(super) fn load(&self) -> Result<Joined, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Joined::default()),
            Err(err) => return Err(format!("{}: {err}", self.path.display())),
        };
        let doc: serde_json::Value =
            serde_json::from_str(&text).map_err(|err| format!("{}: {err}", self.path.display()))?;
        let boxes = string_map(doc.get("boxes"));
        let mut minted = string_set(doc.get("minted"));
        // Joined boxes are ours even when an older file did not list them.
        minted.extend(boxes.values().cloned());
        Ok(Joined { boxes, minted })
    }

    pub(super) fn save(&self, joined: &Joined) -> Result<(), String> {
        let minted: Vec<&String> = joined.minted.iter().collect();
        let doc = serde_json::json!({
            "schema_version": 1,
            "boxes": joined.boxes,
            "minted": minted,
        });
        write_file(&self.path, &format!("{doc:#}\n"))
    }
}

fn string_map(value: Option<&serde_json::Value>) -> BTreeMap<String, String> {
    value
        .and_then(serde_json::Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(id, name)| Some((id.clone(), name.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn string_set(value: Option<&serde_json::Value>) -> BTreeSet<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `binary` is the cached phux for `tag`.
pub(super) fn cached_box_binary(binary: &Path, version_file: &Path, tag: &str) -> bool {
    binary.is_file() && std::fs::read_to_string(version_file).is_ok_and(|text| text.trim() == tag)
}

/// Install `tag`'s Linux phux for `target` under `root`, reusing the cache
/// when it is already that tag. `fetch` returns a directory holding `phux`
/// and runs only when the cache is missing or a different version.
pub(super) fn install_box_binary(
    root: &Path,
    target: &str,
    tag: &str,
    fetch: impl FnOnce(&Path) -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root)
        .map_err(|err| format!("could not create {}: {err}", root.display()))?;
    let dest = root.join(format!("phux-{target}"));
    let version_file = root.join(format!("phux-{target}.version"));
    if cached_box_binary(&dest, &version_file, tag) {
        return Ok(dest);
    }
    let staging = root.join(format!(".download-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|err| format!("could not create {}: {err}", staging.display()))?;
    let fetched = fetch(&staging).and_then(|staged| {
        std::fs::rename(staged.join("phux"), &dest)
            .map_err(|err| format!("could not move the download to {}: {err}", dest.display()))?;
        write_file(&version_file, &format!("{tag}\n"))?;
        Ok(dest)
    });
    let _ = std::fs::remove_dir_all(&staging);
    fetched
}

/// Download, verify, and cache this hub's own phux for `target` (not the
/// channel's later release), refreshing the cache when its version differs.
pub(super) fn fetch_box_binary(target: &str) -> Result<PathBuf, String> {
    let tag = crate::commands::update::own_build()
        .map_err(|err| err.to_string())?
        .tag();
    install_box_binary(&state_root(), target, &tag, |staging| {
        info!("downloading phux {tag} for {target} boxes");
        crate::commands::update::fetch_published(target, staging)
            .map(|(_build, staged)| staged)
            .map_err(|err| err.to_string())
    })
}
