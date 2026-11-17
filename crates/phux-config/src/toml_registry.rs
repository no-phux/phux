//! Comment-preserving `[[remote]]` / `[[satellites]]` registry edits
//! (ADR-0038, ADR-0055) for the CLI and native embedders.
//!
//! [`edit_document`] opens the shared [`ConfigDocument`] transaction with
//! the production guard, following an existing symlink: one target for the
//! sibling advisory lock, read, modify and publish, refusing a dangling link
//! and an observed external change. See [`crate::document`].
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

pub use crate::document::ConfigDocument as RegistryEdit;
use crate::document::{ConfigDocument, Guard, SymlinkPolicy};

/// The refusal a registry writer sees when the file changed under it.
const REGISTRY_STALE: &str = "machine registry changed; refresh Machines";

impl ConfigDocument {
    /// Remove one exact root machine, never an inherited or ambiguous entry.
    /// # Errors
    /// Refuses unknown roles, missing entries and duplicate name/endpoint pairs.
    pub fn remove_machine(&mut self, role: &str, name: &str, endpoint: &str) -> Result<(), String> {
        if !matches!(role, "remote" | "satellites") {
            return Err("unknown machine registry role".to_owned());
        }
        let tables = tables_mut(self, role)?;
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
    let document =
        ConfigDocument::open(path, Guard::ProductionState, SymlinkPolicy::FollowExisting)?;
    Ok(document.with_stale_message(REGISTRY_STALE))
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
    if edit.original() != expected {
        return Err(REGISTRY_STALE.to_owned());
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
mod tests;
