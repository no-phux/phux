//! A managed entry in `extends`, identified by a TOML comment rather than a
//! new schema key. Only that entry is replaced/removed; arbitrary layers stay.

use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Value};

const MARKER: &str = "# phux-managed-distro";

/// Read the selected managed distro without loading it (so a broken or removed
/// distro can still be inspected and disabled). Arbitrary extends are not owned.
///
/// # Errors
/// Unreadable/malformed user config, malformed extends, or ambiguous markers.
pub fn current_distro(path: &Path) -> Result<Option<PathBuf>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };
    let doc = text.parse::<DocumentMut>().map_err(|err| err.to_string())?;
    selected_path(&doc, &text, path)
}

/// Select an absolute distro layer, or disable the managed slot with `None`.
///
/// Preserves comments, personal overrides and other extends. An old
/// `config init --distro` scaffold is adopted only when its generated header
/// identifies an exact extends entry. A new slot goes first, beneath arbitrary
/// user layers. Existing slots keep their precedence. The complete candidate
/// config and the selected layer are validated before an atomic locked write.
/// No server is contacted: callers must explicitly reload running clients.
///
/// # Errors
/// Invalid layers/configs, ambiguous ownership, concurrent edits, or I/O errors.
pub fn switch_distro(path: &Path, distro: Option<&Path>) -> Result<(), String> {
    let distro = distro.map(validate_layer).transpose()?;
    let mut doc = crate::toml_registry::edit_document(path)?;
    let original = doc.to_string();
    let selected = managed_index(&doc, &original)?;
    if distro.is_none() && selected.is_none() {
        return Ok(());
    }
    edit_slot(&mut doc, selected, distro.as_deref())?;
    let text = doc.to_string();
    validate_config(&text, path)?;
    if text != original {
        doc.commit()?;
    }
    Ok(())
}

fn validate_layer(path: &Path) -> Result<PathBuf, String> {
    let path = path
        .canonicalize()
        .map_err(|err| format!("distro layer {}: {err}", path.display()))?;
    let text = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let config = validate_config(&text, &path)?;
    validate_plugins(&config, &path)?;
    Ok(path)
}

fn validate_plugins(config: &crate::Config, path: &Path) -> Result<(), String> {
    for entry in config.plugins.iter().filter(|entry| entry.enabled) {
        let manifest = crate::plugin::resolve_manifest_path(&entry.manifest, path);
        crate::plugin::load_plugin_manifest(&manifest)
            .map_err(|err| format!("distro plugin {}: {err}", manifest.display()))?;
    }
    Ok(())
}

fn validate_config(text: &str, path: &Path) -> Result<crate::Config, String> {
    let config = crate::parse_with_defaults(text, path).map_err(|err| err.to_string())?;
    let report = crate::check::check(text, path).map_err(|err| err.to_string())?;
    if let Some(finding) = report.findings.first() {
        return Err(format!(
            "{}: {}: {}",
            finding.origin(),
            finding.path,
            finding.message
        ));
    }
    Ok(config)
}

fn extends(doc: &DocumentMut) -> Result<Option<&Array>, String> {
    doc.get("extends")
        .map(|item| {
            let array = item
                .as_array()
                .ok_or("extends must be an array of strings")?;
            if array.iter().any(|value| value.as_str().is_none()) {
                return Err("extends must be an array of strings".to_owned());
            }
            Ok(array)
        })
        .transpose()
}

fn managed_index(doc: &DocumentMut, text: &str) -> Result<Option<usize>, String> {
    let Some(array) = extends(doc)? else {
        return Ok(None);
    };
    let indices: Vec<_> = array
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            value
                .decor()
                .prefix()
                .and_then(toml_edit::RawString::as_str)
                .is_some_and(|prefix| prefix.lines().any(|line| line.trim() == MARKER))
                .then_some(index)
        })
        .collect();
    match indices.as_slice() {
        [] => Ok(legacy_index(array, text)),
        [index] => Ok(Some(*index)),
        _ => Err("multiple phux-managed-distro entries; edit extends to keep just one".to_owned()),
    }
}

fn legacy_index(array: &Array, text: &str) -> Option<usize> {
    if !text.starts_with("# phux configuration, scaffolded on top of a starter distribution.\n") {
        return None;
    }
    let named = text
        .lines()
        .take_while(|line| line.starts_with('#') || line.is_empty())
        .find_map(|line| line.strip_prefix("#   "))?;
    array.iter().position(|value| value.as_str() == Some(named))
}

fn selected_path(doc: &DocumentMut, text: &str, path: &Path) -> Result<Option<PathBuf>, String> {
    let Some(index) = managed_index(doc, text)? else {
        return Ok(None);
    };
    let value = extends(doc)?
        .and_then(|array| array.get(index))
        .and_then(Value::as_str)
        .ok_or("managed distro entry is not a string")?;
    let layer = Path::new(value);
    Ok(Some(
        path.parent().unwrap_or_else(|| Path::new(".")).join(layer),
    ))
}

fn edit_slot(
    doc: &mut DocumentMut,
    index: Option<usize>,
    distro: Option<&Path>,
) -> Result<(), String> {
    if doc.get("extends").is_none() {
        let mut array = Array::new();
        array.set_trailing("\n");
        doc.as_table_mut()
            .insert("extends", toml_edit::value(array));
    }
    let array = doc
        .get_mut("extends")
        .and_then(toml_edit::Item::as_array_mut)
        .ok_or("extends must be an array of strings")?;
    let Some(distro) = distro else {
        if let Some(index) = index {
            remove_slot(array, index);
        }
        return Ok(());
    };
    let path = distro.to_str().ok_or("distro path is not UTF-8")?;
    let mut value = Value::from(path);
    if let Some(index) = index {
        let previous = array.get(index).ok_or("managed slot disappeared")?;
        *value.decor_mut() = previous.decor().clone();
        ensure_marker(&mut value);
        array.replace_formatted(index, value);
    } else {
        ensure_marker(&mut value);
        array.insert_formatted(0, value);
    }
    Ok(())
}

fn remove_slot(array: &mut Array, index: usize) {
    let removed = array.remove(index);
    let prefix = removed
        .decor()
        .prefix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or("");
    let comments = prefix
        .lines()
        .filter(|line| line.trim() != MARKER)
        .collect::<Vec<_>>()
        .join("\n");
    if !comments.contains('#') {
        return;
    }
    // A user's notes on the selected entry outlive switching it off. Do not
    // preserve the managed marker, which would claim the next unrelated entry.
    if let Some(next) = array.get_mut(index) {
        let next_prefix = next
            .decor()
            .prefix()
            .and_then(toml_edit::RawString::as_str)
            .unwrap_or("");
        let prefix = format!("{comments}\n{next_prefix}");
        next.decor_mut().set_prefix(prefix);
    } else {
        let trailing = array.trailing().as_str().unwrap_or("");
        let trailing = format!("{comments}\n{trailing}");
        array.set_trailing(trailing);
    }
}

fn ensure_marker(value: &mut Value) {
    let prefix = value
        .decor()
        .prefix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or("");
    if !prefix.lines().any(|line| line.trim() == MARKER) {
        let marked = format!("{prefix}\n  {MARKER}\n  ");
        value.decor_mut().set_prefix(marked);
    }
}
