//! `min_phux_version` gate, enforced by [`super::load_plugin_manifest`] so
//! every consumer that loads a manifest is covered.

use super::PluginManifestError;

/// The phux version manifests are gated against (the shared workspace
/// version).
pub const CURRENT_PHUX_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Reject a manifest whose `min_phux_version` is newer than `current`,
/// naming both versions.
pub(super) fn enforce_min_phux_version(
    plugin_id: &str,
    min_phux_version: &str,
    current: &str,
) -> Result<(), PluginManifestError> {
    let min = parse_version(min_phux_version).ok_or_else(|| {
        PluginManifestError::Invalid(format!(
            "plugin {plugin_id} declares malformed min_phux_version \
             {min_phux_version:?} (expected a dotted numeric version like \"0.1.0\")"
        ))
    })?;
    let have = parse_version(current).ok_or_else(|| {
        PluginManifestError::Invalid(format!(
            "current phux version {current:?} is not a dotted numeric version"
        ))
    })?;
    if min > have {
        return Err(PluginManifestError::Invalid(format!(
            "plugin {plugin_id} requires phux >= {min_phux_version}, \
             but this is phux {current}"
        )));
    }
    Ok(())
}

/// Parse `"X"`, `"X.Y"`, or `"X.Y.Z"` (missing components are zero).
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = [0_u64; 3];
    let mut count = 0;
    for part in text.trim().split('.') {
        if count == parts.len() || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parts[count] = part.parse().ok()?;
        count += 1;
    }
    (count > 0).then_some(parts.into())
}

#[cfg(test)]
mod tests {
    use super::{enforce_min_phux_version, parse_version};

    #[test]
    fn versions_parse_as_one_to_three_numeric_components() {
        assert_eq!(parse_version("1"), Some((1, 0, 0)));
        assert_eq!(parse_version("0.2"), Some((0, 2, 0)));
        assert_eq!(parse_version(" 1.2.3 "), Some((1, 2, 3)));
        for bad in ["", ".", "1.", ".1", "1.2.3.4", "abc", "1.x", "1.2-rc1"] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_floor_admits_older_and_names_both_versions_when_newer() {
        for min in ["0.0.3", "0.0.2", "0"] {
            assert!(enforce_min_phux_version("p", min, "0.0.3").is_ok(), "{min}");
        }
        let newer = enforce_min_phux_version("example.future", "9.9.9", "0.0.3");
        let message = newer.map_err(|e| e.to_string()).unwrap_err();
        for needle in ["example.future", "9.9.9", "0.0.3"] {
            assert!(message.contains(needle), "{message}");
        }
        let malformed = enforce_min_phux_version("example.bad", "not-a-version", "0.0.3");
        let message = malformed.map_err(|e| e.to_string()).unwrap_err();
        assert!(message.contains("malformed min_phux_version"), "{message}");
    }
}
