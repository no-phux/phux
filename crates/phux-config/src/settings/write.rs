//! Set or unset one dotted key in the user's `config.toml`, keeping every
//! other byte (ADR-0023: the file is the source of truth).
//!
//! Edits go through `toml_edit`: an existing value is replaced in place
//! (trailing comment intact), a missing key or table is appended. Every edit
//! is validated (it must load, and `phux config check` must report nothing at
//! the key) before [`write_edit`] touches disk. Writes go through the
//! shared [`ConfigDocument`] transaction, so a settings edit and a
//! concurrent `phux host add` cannot drop each other's change.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, TableLike};

use crate::ConfigError;
use crate::document::{ConfigDocument, DocumentError, Guard, SymlinkPolicy};

/// What to do to one key.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Assign this value, creating the key and its table as needed.
    Set(toml::Value),
    /// Remove the key from the user's file so the layers beneath show
    /// through. Removing an absent key is a no-op success.
    Unset,
}

/// What an edit did.
#[derive(Debug, Clone, PartialEq)]
pub struct EditOutcome {
    /// The whole file after the edit.
    pub text: String,
    /// What the *user's file* held at the key before the edit — not the
    /// merged effective value. `None` when the key was absent.
    pub previous: Option<toml::Value>,
    /// For a `Set`, the assignment line now in the file (leading indent and
    /// newline trimmed; a trailing comment kept), e.g. `width = 32`. `None`
    /// for an `Unset`.
    pub line: Option<String>,
}

/// Apply `edit` to `key` (`table.leaf`, bare segments) in `user_toml` and
/// validate the result, without writing. `path` names the file in errors and
/// anchors relative `extends` entries.
///
/// # Errors
///
/// [`ConfigError::Parse`] when the input or the edited result does not load;
/// [`ConfigError::Edit`] for a malformed key, a path through a non-table, a
/// table leaf, a date-time value, or a `phux config check` finding at the
/// key.
pub fn apply_edit(
    user_toml: &str,
    key: &str,
    edit: Edit,
    path: &Path,
) -> Result<EditOutcome, ConfigError> {
    split_key(key)?;
    let mut doc = parse_document(user_toml, path)?;
    let outcome = apply_edit_doc(&mut doc, key, edit, path)?;
    validate(&outcome.text, &[key], path)?;
    Ok(outcome)
}

/// Apply `edit` to `key` in `doc`, without validating the result. The
/// outcome's `text` is the whole document after the edit.
fn apply_edit_doc(
    doc: &mut DocumentMut,
    key: &str,
    edit: Edit,
    path: &Path,
) -> Result<EditOutcome, ConfigError> {
    let (table_path, leaf) = split_key(key)?;
    let previous = previous_value(&doc.to_string(), key, path)?;

    let line = match edit {
        Edit::Set(value) => {
            let new_value = to_edit_value(&value, key)?;
            let document_was_empty = doc.as_table().is_empty();
            let table = table_for_set(doc, table_path, key, document_was_empty)?;
            set_leaf(table, leaf, key, new_value)?;
            table
                .get_key_value(leaf)
                .map(|(k, item)| render_line(k, item))
        }
        Edit::Unset => {
            unset_leaf(doc, table_path, leaf, key)?;
            None
        }
    };

    Ok(EditOutcome {
        text: doc.to_string(),
        previous,
        line,
    })
}

/// Split `key` into its table path and leaf, refusing a top-level key and
/// an empty segment.
fn split_key(key: &str) -> Result<(&str, &str), ConfigError> {
    let (table_path, leaf) = key.rsplit_once('.').ok_or_else(|| {
        edit_error(
            key,
            "a setting lives inside a table; use `<table>.<key>` (there are no top-level \
             settings)",
        )
    })?;
    if key.split('.').any(str::is_empty) {
        return Err(edit_error(key, "empty path segment"));
    }
    Ok((table_path, leaf))
}

/// Read `path` (missing is empty), apply `edit`, and replace the file.
///
/// The edit is one [`ConfigDocument`] transaction: settings are shared by
/// every profile ([`Guard::SharedSettings`]), and a symlink is followed so
/// the link survives ([`SymlinkPolicy::FollowExisting`]). The write goes
/// through an fsynced sibling temp file and a rename, the target's
/// permissions are kept, and a no-op edit does not touch the file. Another
/// phux holding the lock is waited for at most
/// [`crate::document::LOCK_WAIT`], then reported as busy.
///
/// # Errors
///
/// Everything [`apply_edit`] returns (the file is untouched);
/// [`ConfigError::Write`] when the file cannot be locked, read, or
/// replaced, or changed on disk since it was read.
pub fn write_edit(path: &Path, key: &str, edit: Edit) -> Result<EditOutcome, ConfigError> {
    let mut doc = open_settings(path)?;
    let outcome = apply_edit_doc(&mut doc, key, edit, path)?;
    validate(&outcome.text, &[key], path)?;
    publish_if_changed(doc, path)?;
    Ok(outcome)
}

/// Apply every edit in order and publish them as one transaction.
///
/// Either all of them land or none does. Only the final file is validated,
/// so an intermediate state (one theme key unset before the other is set)
/// never has to load on its own.
///
/// # Errors
///
/// As [`write_edit`], for any of the edits.
pub fn write_edits(path: &Path, edits: Vec<(&str, Edit)>) -> Result<(), ConfigError> {
    let mut doc = open_settings(path)?;
    let mut keys = Vec::with_capacity(edits.len());
    for (key, edit) in edits {
        apply_edit_doc(&mut doc, key, edit, path)?;
        keys.push(key);
    }
    validate(&doc.to_string(), &keys, path)?;
    publish_if_changed(doc, path)
}

/// Open the settings transaction on `path`.
fn open_settings(path: &Path) -> Result<ConfigDocument, ConfigError> {
    ConfigDocument::open(path, Guard::SharedSettings, SymlinkPolicy::FollowExisting).map_err(
        |err| match err {
            DocumentError::Parse(failure) => ConfigError::parse(
                path,
                &failure.text,
                failure.error.span(),
                failure.error.message(),
            ),
            DocumentError::Unavailable(message) => {
                write_error(path, std::io::Error::other(message))
            }
        },
    )
}

/// Publish `doc` unless the edits left its text as it was read.
fn publish_if_changed(doc: ConfigDocument, path: &Path) -> Result<(), ConfigError> {
    if !doc.is_changed() {
        return Ok(());
    }
    doc.publish().map_err(|source| write_error(path, source))
}

/// A [`ConfigError::Write`] for `path`.
fn write_error(path: &Path, source: std::io::Error) -> ConfigError {
    ConfigError::Write {
        path: path.to_path_buf(),
        source,
    }
}

/// A [`ConfigError::Edit`] for `key`.
fn edit_error(key: &str, message: impl Into<String>) -> ConfigError {
    ConfigError::Edit {
        key: key.to_owned(),
        message: message.into(),
    }
}

/// Parse the user's text as a mutable, formatting-preserving document.
fn parse_document(user_toml: &str, path: &Path) -> Result<DocumentMut, ConfigError> {
    user_toml
        .parse::<DocumentMut>()
        .map_err(|err| ConfigError::parse(path, user_toml, err.span(), err.message()))
}

/// What the user's file holds at `key` before the edit, read through the
/// plain `toml` parser (the two crates' date-time types do not convert).
fn previous_value(
    user_toml: &str,
    key: &str,
    path: &Path,
) -> Result<Option<toml::Value>, ConfigError> {
    let table = crate::layer::parse_table(user_toml, path)?;
    Ok(super::value_at(&table, key).cloned())
}

/// Convert a `toml::Value` into a `toml_edit::Value`; date-times are refused
/// (no setting takes one).
fn to_edit_value(value: &toml::Value, key: &str) -> Result<toml_edit::Value, ConfigError> {
    Ok(match value {
        toml::Value::String(s) => toml_edit::Value::from(s.as_str()),
        toml::Value::Integer(i) => toml_edit::Value::from(*i),
        toml::Value::Float(f) => toml_edit::Value::from(*f),
        toml::Value::Boolean(b) => toml_edit::Value::from(*b),
        toml::Value::Datetime(_) => {
            return Err(edit_error(key, "no setting takes a date-time value"));
        }
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(to_edit_value(item, key)?);
            }
            toml_edit::Value::Array(array)
        }
        toml::Value::Table(table) => {
            let mut inline = toml_edit::InlineTable::new();
            for (k, v) in table {
                inline.insert(k.as_str(), to_edit_value(v, key)?);
            }
            toml_edit::Value::InlineTable(inline)
        }
    })
}

/// Walk `table_path`, creating tables as needed; a new top-level table in a
/// non-empty document gets a blank line before its header.
fn table_for_set<'d>(
    doc: &'d mut DocumentMut,
    table_path: &str,
    key: &str,
    document_was_empty: bool,
) -> Result<&'d mut dyn TableLike, ConfigError> {
    // A comment-only document (a `phux config init` scaffold) holds its text
    // as trailing decor, emitted after any appended table: move it onto the
    // new table's prefix so the scaffold stays above the first assignment.
    let mut carried_prefix = if document_was_empty {
        let trailing = doc.trailing().as_str().unwrap_or("").to_owned();
        if trailing.trim().is_empty() {
            None
        } else {
            doc.set_trailing("");
            Some(if trailing.ends_with('\n') {
                trailing
            } else {
                format!("{trailing}\n")
            })
        }
    } else {
        None
    };
    let mut current: &'d mut dyn TableLike = doc.as_table_mut();
    for (depth, segment) in table_path.split('.').enumerate() {
        let item = current.entry(segment).or_insert_with(|| {
            let mut table = Table::new();
            if depth == 0 {
                if let Some(prefix) = carried_prefix.take() {
                    table.decor_mut().set_prefix(prefix);
                } else if !document_was_empty {
                    table.decor_mut().set_prefix("\n");
                }
            }
            Item::Table(table)
        });
        current = item.as_table_like_mut().ok_or_else(|| {
            edit_error(
                key,
                format!("`{segment}` is not a table, so nothing can be set beneath it"),
            )
        })?;
    }
    Ok(current)
}

/// The assignment line as the file now shows it, trimmed. Missing decor is
/// filled the way the table encoder fills it (one space each side of `=`).
fn render_line(key: &toml_edit::Key, item: &Item) -> String {
    let key_suffix = key
        .leaf_decor()
        .suffix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or(" ");
    let value_has_prefix = item
        .as_value()
        .and_then(|value| value.decor().prefix())
        .and_then(toml_edit::RawString::as_str)
        .is_some();
    let value_prefix = if value_has_prefix { "" } else { " " };
    format!("{}{key_suffix}={value_prefix}{item}", key.display_repr())
        .trim()
        .to_owned()
}

/// Assign `value` to `leaf`, in place (keeping decor) when the key exists.
fn set_leaf(
    table: &mut dyn TableLike,
    leaf: &str,
    key: &str,
    mut value: toml_edit::Value,
) -> Result<(), ConfigError> {
    match table.get_mut(leaf) {
        None => {
            table.insert(leaf, Item::Value(value));
        }
        Some(existing) => {
            let slot = existing.as_value_mut().ok_or_else(|| {
                edit_error(key, "holds a table, not a value; edit the file directly")
            })?;
            *value.decor_mut() = slot.decor().clone();
            *slot = value;
        }
    }
    Ok(())
}

/// Remove `leaf` from the table at `table_path`, if both exist. An emptied
/// table is left in place with its comments.
fn unset_leaf(
    doc: &mut DocumentMut,
    table_path: &str,
    leaf: &str,
    key: &str,
) -> Result<(), ConfigError> {
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    for segment in table_path.split('.') {
        let Some(item) = current.get_mut(segment) else {
            return Ok(());
        };
        current = item.as_table_like_mut().ok_or_else(|| {
            edit_error(
                key,
                format!("`{segment}` is not a table, so nothing can be unset beneath it"),
            )
        })?;
    }
    if current.get(leaf).is_some_and(|item| !item.is_value()) {
        return Err(edit_error(
            key,
            "holds a table, not a value; edit the file directly",
        ));
    }
    current.remove(leaf);
    Ok(())
}

/// The gate every edit passes: a `phux config check` finding at an edited
/// key blocks it (findings elsewhere do not), and the whole file must still
/// load.
fn validate(text: &str, keys: &[&str], path: &Path) -> Result<(), ConfigError> {
    let report = crate::check::check(text, path)?;
    if let Some(finding) = report
        .findings
        .iter()
        .find(|f| keys.contains(&f.path.as_str()))
    {
        return Err(edit_error(
            &finding.path,
            format!("{}: {}", finding.fault.label(), finding.message),
        ));
    }
    crate::parse_with_defaults(text, path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
