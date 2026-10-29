//! Comment-preserving `[[remote]]` / `[[satellites]]` registry edits
//! (ADR-0038, ADR-0055) for the CLI and native embedders.
//!
//! [`edit_document`] resolves one target for its sibling advisory lock, read,
//! modify and publish, refusing a busy writer immediately even through a
//! symlink alias. Dangling file symlinks are refused rather than replaced.
//! Publication is atomic and refuses when the file changed since it was read;
//! that check is not an atomic CAS against editors that do not take the lock.
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

/// One locked registry read/modify/publish transaction.
pub struct RegistryEdit {
    path: PathBuf,
    document: DocumentMut,
    original: String,
    _lock: std::fs::File,
}

impl std::fmt::Debug for RegistryEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryEdit")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for RegistryEdit {
    type Target = DocumentMut;
    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

impl std::ops::DerefMut for RegistryEdit {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.document
    }
}

impl RegistryEdit {
    /// Publish this edit while retaining its lock through the rename.
    /// # Errors
    /// Refuses observed external changes and filesystem failures.
    pub fn commit(self) -> Result<(), String> {
        let text = self.document.to_string();
        crate::settings::replace_atomically(&self.path, &text, || {
            if read_text(&self.path).map_err(std::io::Error::other)? != self.original {
                return Err(std::io::Error::other(
                    "machine registry changed; refresh Machines",
                ));
            }
            #[cfg(test)]
            before_publish_hook();
            Ok(())
        })
        .map_err(|err| format!("could not write {}: {err}", self.path.display()))
    }

    /// Remove one exact root machine, never an inherited or ambiguous entry.
    /// # Errors
    /// Refuses unknown roles, missing entries and duplicate name/endpoint pairs.
    pub fn remove_machine(&mut self, role: &str, name: &str, endpoint: &str) -> Result<(), String> {
        if !matches!(role, "remote" | "satellites") {
            return Err("unknown machine registry role".to_owned());
        }
        let tables = tables_mut(&mut self.document, role)?;
        let matches = machine_indices(tables, name, endpoint);
        let [index] = matches.as_slice() else {
            return Err(
                "machine is ambiguous or inherited; edit its source configuration".to_owned(),
            );
        };
        tables.remove(*index);
        Ok(())
    }
}

/// Begin a shared registry mutation, acquiring its lock before reading contents.
/// # Errors
/// Refuses busy writers, malformed documents and filesystem errors.
pub fn edit_document(path: &Path) -> Result<RegistryEdit, String> {
    crate::production::refuse_dev_on_production_state(path)?;
    let path = registry_target(path)?;
    let lock = lock_registry(&path)?;
    let original = read_text(&path)?;
    let document = original
        .parse::<DocumentMut>()
        .map_err(|err| format!("could not parse {}: {err}", path.display()))?;
    Ok(RegistryEdit {
        path,
        document,
        original,
        _lock: lock,
    })
}

/// Existing files use their real target; new files use a real parent plus
/// their filename. Never treat a dangling file symlink as an independent file.
fn registry_target(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| "registry has no file name".to_owned())?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    let resolved = match std::fs::symlink_metadata(path) {
        Ok(_) => std::fs::canonicalize(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::canonicalize(parent).map(|parent| parent.join(name))
        }
        Err(err) => Err(err),
    };
    resolved.map_err(|err| format!("could not resolve registry {}: {err}", path.display()))
}

fn lock_registry(path: &Path) -> Result<std::fs::File, String> {
    // The machine registries point the production client at credentials.
    crate::production::refuse_dev_on_production_state(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| "registry has no parent directory".to_owned())?;
    let mut name = path
        .file_name()
        .ok_or_else(|| "registry has no file name".to_owned())?
        .to_os_string();
    name.push(".registry.lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(name))
        .map_err(|err| err.to_string())?;
    // Another phux editing the registry holds it only for a read and a
    // write, and a child process being spawned holds an inherited lock only
    // until it execs: wait that out rather than fail on a brief overlap.
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(err) => {
                return Err(format!(
                    "registry is busy or could not be locked; retry: {err}"
                ));
            }
        }
    }
}

/// How long a registry edit waits for another holder of the lock.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

fn read_text(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(input) => Ok(input),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("could not read {}: {err}", path.display())),
    }
}

/// Remove one captured machine entry in the root user file, preserving comments.
///
/// `expected` is the exact root document captured when the entry was displayed.
/// Inherited entries are not writable through this seam. Roles use the config's
/// keys (`remote` or `satellites`), never a combined inventory row offset.
///
/// # Errors
/// Refuses changed, malformed, missing, ambiguous or inherited entries.
pub fn forget_machine(
    path: &Path,
    expected: &str,
    role: &str,
    name: &str,
    endpoint: &str,
) -> Result<(), String> {
    let mut edit = edit_document(path)?;
    if edit.original != expected {
        return Err("machine registry changed; refresh Machines".to_owned());
    }
    edit.remove_machine(role, name, endpoint)?;
    edit.commit()
}

/// Insert a new `key` entry, or rewrite the one at the index `existing`
/// finds, then publish, all under one registry lock.
///
/// `existing` runs after the lock is held, so the index it resolves cannot
/// go stale before the write. `fill` writes the whole entry either way.
///
/// # Errors
/// Lock, parse, lookup and publish failures.
pub fn upsert_machine(
    path: &Path,
    key: &str,
    existing: impl FnOnce() -> Result<Option<usize>, String>,
    fill: impl FnOnce(&mut Table),
) -> Result<(), String> {
    let mut doc = edit_document(path)?;
    if let Some(index) = existing()? {
        fill(table_mut(&mut doc, key, index)?);
    } else {
        let mut table = Table::new();
        fill(&mut table);
        tables_mut(&mut doc, key)?.push(table);
    }
    doc.commit()
}

/// Remove the one root `key` entry matching `name` and `endpoint`, under the
/// registry lock. See [`RegistryEdit::remove_machine`].
///
/// # Errors
/// Lock, parse, ambiguity and publish failures.
pub fn remove_machine_entry(
    path: &Path,
    key: &str,
    name: &str,
    endpoint: &str,
) -> Result<(), String> {
    let mut doc = edit_document(path)?;
    doc.remove_machine(key, name, endpoint)?;
    doc.commit()
}

/// Indices of every entry whose `name` and `endpoint` both match exactly.
#[must_use]
pub fn machine_indices(tables: &ArrayOfTables, name: &str, endpoint: &str) -> Vec<usize> {
    tables
        .iter()
        .enumerate()
        .filter_map(|(index, table)| {
            (table.get("name").and_then(Item::as_str) == Some(name)
                && table.get("endpoint").and_then(Item::as_str) == Some(endpoint))
            .then_some(index)
        })
        .collect()
}

/// Write `key`, or remove it when `value` is `None`, so rewriting an entry
/// never keeps a stale field the new entry omitted.
pub fn set_or_remove<V: Into<Value>>(table: &mut Table, key: &str, value: Option<V>) {
    match value {
        Some(value) => {
            table.insert(key, toml_edit::value(value));
        }
        None => {
            table.remove(key);
        }
    }
}

/// Refuse a second entry with an already-seen name: one name must resolve
/// to exactly one machine. `role` names the registry in the error.
///
/// # Errors
/// `duplicate {role} name "..."` for the first repeated name.
pub fn reject_duplicate_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
    role: &str,
) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(format!("duplicate {role} name {name:?}"));
        }
    }
    Ok(())
}

/// The trimmed machine name, or `None` when it is empty or contains `/` or
/// `:` (the separators of the endpoint and selector grammars). Each registry
/// adds its own rules and error wording on top.
#[must_use]
pub fn plain_machine_name(name: &str) -> Option<&str> {
    let trimmed = name.trim();
    (!trimmed.is_empty() && !trimmed.contains('/') && !trimmed.contains(':')).then_some(trimmed)
}

/// Require an absolute token-file path.
///
/// The file is read later by a process whose working directory is unrelated to where it was registered, so a
/// relative path would silently point elsewhere. Existence is not required:
/// pairing material may land after registration.
///
/// # Errors
/// `{role} token-file must be an absolute path (got ...)`.
pub fn validate_token_file(path: &Path, role: &str) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(format!(
            "{role} token-file must be an absolute path (got {})",
            path.display()
        ));
    }
    Ok(path.to_path_buf())
}

/// The array of tables under `key`, creating it when absent.
pub fn tables_mut<'doc>(
    doc: &'doc mut DocumentMut,
    key: &str,
) -> Result<&'doc mut ArrayOfTables, String> {
    let item = doc
        .as_table_mut()
        .entry(key)
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
    item.as_array_of_tables_mut()
        .ok_or_else(|| format!("`{key}` must be an array of tables"))
}

/// One entry of the array of tables under `key`.
pub fn table_mut<'doc>(
    doc: &'doc mut DocumentMut,
    key: &str,
    index: usize,
) -> Result<&'doc mut Table, String> {
    tables_mut(doc, key)?
        .get_mut(index)
        .ok_or_else(|| format!("`{key}` registry index {index} disappeared"))
}

/// Validate a SHA-256 certificate pin: 64 hex digits, optionally with the
/// `AB:CD:...` separators `phux pair` prints. Catching a truncated paste at
/// registration beats a baffling handshake failure later.
pub fn validate_fingerprint(fingerprint: &str, what: &str) -> Result<String, String> {
    let trimmed = fingerprint.trim();
    let hex_digits = trimmed.chars().filter(char::is_ascii_hexdigit).count();
    let separators_only = trimmed
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == ':' || c.is_whitespace());
    if hex_digits == 64 && separators_only {
        Ok(trimmed.to_owned())
    } else {
        Err(format!(
            "{what} cert-fingerprint must be a SHA-256 fingerprint (64 hex digits, \
             optionally colon-separated) as printed by `phux pair`"
        ))
    }
}

#[cfg(test)]
thread_local! {
    // Deterministic scheduler boundary, not an alternative writer implementation.
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn before_publish_hook() {
    let hook = BEFORE_PUBLISH.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
mod tests;
