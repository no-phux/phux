use std::collections::BTreeSet;

use super::PluginManifestError;

const PLUGIN_ID_MAX_CHARS: usize = 120;
const ENTRY_ID_MAX_CHARS: usize = 120;

pub(super) fn normalize_command(command: &[String]) -> Result<Vec<String>, PluginManifestError> {
    if command.is_empty() {
        return Err(PluginManifestError::Invalid(
            "plugin command must not be empty".to_owned(),
        ));
    }
    command
        .iter()
        .map(|part| non_empty(part, "plugin command part"))
        .collect()
}

pub(super) fn normalize_id(
    value: &str,
    allow_dot: bool,
    label: &str,
) -> Result<String, PluginManifestError> {
    let value = value.trim();
    let valid = !value.is_empty()
        && value.len() <= PLUGIN_ID_MAX_CHARS
        && !reserved_path_id(value, label)
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || ch == '_'
                || ch == '-'
                || ch == ':'
                || (allow_dot && ch == '.')
        });
    if valid {
        Ok(value.to_owned())
    } else {
        Err(PluginManifestError::Invalid(format!("invalid {label}")))
    }
}

/// Plugin ids and startup ids are path components under `plugin-startup/`.
/// A dot-only id (`.` or `..`) escapes that directory. `server` collides
/// with files the supervisor writes beside the sidecar.
fn reserved_path_id(value: &str, label: &str) -> bool {
    if label != "plugin id" && label != "plugin startup id" {
        return false;
    }
    value.eq_ignore_ascii_case("server") || value.chars().all(|ch| ch == '.')
}

pub(super) fn non_empty(value: &str, label: &str) -> Result<String, PluginManifestError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        Err(PluginManifestError::Invalid(format!("{label} is required")))
    } else {
        Ok(value)
    }
}

pub(super) fn trim_optional(value: &str) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

pub(super) fn reject_duplicate_ids<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    label: &str,
) -> Result<(), PluginManifestError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if id.len() > ENTRY_ID_MAX_CHARS || !seen.insert(id) {
            return Err(PluginManifestError::Invalid(format!(
                "duplicate {label} id '{id}'"
            )));
        }
    }
    Ok(())
}
