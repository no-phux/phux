//! The files `phux host minimal` owns: the Minimal loadout and the
//! `default_loadouts` entry in Minimal's config, the record of which box
//! joined as which satellite, and the cached Linux phux boxes get.

use std::collections::BTreeMap;
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

/// Which Minimal session joined as which satellite:
/// `{"schema_version":1,"boxes":{"<session id>":"<satellite>"}}`.
#[derive(Debug, Clone)]
pub(super) struct BoxRegistry {
    path: PathBuf,
}

impl BoxRegistry {
    pub(super) const fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(super) fn load(&self) -> Result<BTreeMap<String, String>, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => return Err(format!("{}: {err}", self.path.display())),
        };
        let doc: serde_json::Value =
            serde_json::from_str(&text).map_err(|err| format!("{}: {err}", self.path.display()))?;
        Ok(doc
            .get("boxes")
            .and_then(serde_json::Value::as_object)
            .map(|boxes| {
                boxes
                    .iter()
                    .filter_map(|(id, name)| Some((id.clone(), name.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub(super) fn save(&self, boxes: &BTreeMap<String, String>) -> Result<(), String> {
        let doc = serde_json::json!({ "schema_version": 1, "boxes": boxes });
        write_file(&self.path, &format!("{doc:#}\n"))
    }
}

/// Download, verify, and cache the published phux for `target`, replacing
/// the cached copy, and return its path.
pub(super) fn fetch_box_binary(target: &str) -> Result<PathBuf, String> {
    let root = state_root();
    let staging = root.join(format!(".download-{}", std::process::id()));
    std::fs::create_dir_all(&staging)
        .map_err(|err| format!("could not create {}: {err}", staging.display()))?;
    let fetched = crate::commands::update::fetch_published(target, &staging)
        .map_err(|err| err.to_string())
        .and_then(|(entry, staged)| {
            let dest = root.join(format!("phux-{target}"));
            std::fs::rename(staged.join("phux"), &dest).map_err(|err| {
                format!("could not move the download to {}: {err}", dest.display())
            })?;
            info!("box phux: {}", entry.build.tag());
            Ok(dest)
        });
    let _ = std::fs::remove_dir_all(&staging);
    fetched
}
