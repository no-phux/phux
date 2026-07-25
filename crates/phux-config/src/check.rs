//! Config validation that says *where* the problem is.
//!
//! [`crate::Config`] already carries `#[serde(deny_unknown_fields)]`, so a
//! typo is rejected rather than silently ignored. What it is not is
//! *locatable*. serde names only the leaf field:
//!
//! ```text
//! config.toml: 1:1: unknown field `enabledd`, expected one of `enabled`, `width`, `position`
//! ```
//!
//! Three things are wrong with that, and this module fixes all three.
//!
//! 1. **No parent path.** `enabledd` does not say which table it is in, and
//!    several tables share key names (`enabled`, `width`, `position`). This
//!    module reports `sidebar.enabledd`, derived from the schema walk by
//!    [`serde_path_to_error`] — so it cannot drift the way a hand-maintained
//!    key list would.
//! 2. **A false position.** The `1:1` is not where the typo is. It is the
//!    fallback for a deserialize error carrying no span, because the value
//!    being deserialized is the *merged* layer stack, not the user's text.
//!    Reporting a confident, wrong line number is worse than reporting none.
//!    This module attributes each finding to the layer that introduced it
//!    (ADR-0039) instead, which with `extends` in play is the question the
//!    operator actually has: is the typo mine, or the distro's?
//! 3. **One at a time.** The loader stops at the first bad key, so fixing a
//!    config with four typos takes four edit-run cycles. This module removes
//!    each offending key and re-walks, collecting every finding in one pass.

use std::path::Path;

use serde_path_to_error::Segment;

use crate::{Config, ConfigError, LayerSource, merged_config_with_provenance};

/// Upper bound on findings collected in one run.
///
/// Each finding costs one full re-walk of the merged table, so a config with
/// hundreds of bad keys would otherwise spend quadratic time telling the
/// operator something they learned from the first twenty. Reaching the cap is
/// reported, never silently truncated.
const MAX_FINDINGS: usize = 64;

/// What kind of mistake a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The schema has no such key: a typo, or a key removed in a later
    /// version.
    UnknownKey,
    /// The key exists but the value has the wrong type or shape.
    BadValue,
}

impl Fault {
    /// Short label for human output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UnknownKey => "unknown key",
            Self::BadValue => "bad value",
        }
    }
}

/// One problem found in the resolved config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Dotted path to the key, as TOML addresses it (e.g. `sidebar.enabledd`).
    pub path: String,
    /// Whether the key is unknown or merely wrong.
    pub fault: Fault,
    /// serde's own message, which carries the `expected one of ...` list.
    pub message: String,
    /// The layer that introduced the key, when it can be attributed.
    ///
    /// `None` when provenance has no leaf entry for the path — the case for
    /// an unknown *table*, whose leaves are recorded under their own longer
    /// paths rather than under the table name.
    pub source: Option<LayerSource>,
}

impl Finding {
    /// Human-readable origin: the layer's file, or a stable label for the
    /// embedded defaults and for findings that could not be attributed.
    #[must_use]
    pub fn origin(&self) -> String {
        match &self.source {
            Some(LayerSource::Defaults) => "<embedded default.toml>".to_owned(),
            Some(LayerSource::Extended(p) | LayerSource::User(p)) => p.display().to_string(),
            None => "unattributed".to_owned(),
        }
    }
}

/// The verdict for one config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// Every problem found, in discovery order.
    pub findings: Vec<Finding>,
    /// Whether the internal finding cap was reached, so the caller can say
    /// the list is partial rather than implying the config is now clean.
    pub truncated: bool,
}

impl CheckReport {
    /// Whether the config is clean.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Check a config's whole resolved layer stack.
///
/// `user_input` and `path` are the pair [`crate::parse_with_defaults`] takes:
/// the root config's text and its path, the latter both for error reporting
/// and as the base directory for relative `extends` entries.
///
/// # Errors
///
/// Propagates [`ConfigError`] only for failures that stop the check from
/// running at all — TOML that does not parse, or a layer that cannot be read
/// or is cyclic. "No findings" must never be reported for a file that was
/// never successfully read.
pub fn check(user_input: &str, path: &Path) -> Result<CheckReport, ConfigError> {
    let (mut merged, provenance) = merged_config_with_provenance(user_input, path)?;

    let mut findings = Vec::new();
    let mut truncated = false;

    loop {
        // Deserializing a clone each round is the price of collecting more
        // than one finding: the walk consumes the value, and the offending
        // key has to be removed from the original before the next attempt.
        let attempt: Result<Config, _> =
            serde_path_to_error::deserialize(toml::Value::Table(merged.clone()));
        let Err(err) = attempt else {
            break;
        };

        let segments: Vec<&Segment> = err.path().iter().collect();
        // serde's Display appends its own "in `<table>`" context on a second
        // line. That is exactly what `path` already says, and it turns every
        // finding into a three-line paragraph, so keep the first line only.
        let raw = err.inner().to_string();
        let message = raw.lines().next().unwrap_or(&raw).to_owned();
        let fault = if message.starts_with("unknown field") {
            Fault::UnknownKey
        } else {
            Fault::BadValue
        };
        let key_path = err.path().to_string();

        // A finding we cannot locate in the table cannot be removed, so the
        // next round would rediscover it forever. Record it and stop rather
        // than spin: an incomplete list is recoverable, a hang is not.
        if !remove_at(&mut merged, &segments) {
            findings.push(Finding {
                path: key_path,
                fault,
                message,
                source: None,
            });
            break;
        }

        let source = attribute(&key_path, &provenance);
        findings.push(Finding {
            path: key_path,
            fault,
            message,
            source,
        });

        if findings.len() >= MAX_FINDINGS {
            truncated = true;
            break;
        }
    }

    Ok(CheckReport {
        findings,
        truncated,
    })
}

/// Remove the value addressed by `segments` from `table`.
///
/// Returns whether something was actually removed. Only map segments are
/// navigable: a fault inside an array element is reported against the
/// element's path but cannot be surgically excised without renumbering the
/// array, so those stop the walk (see the caller).
fn remove_at(table: &mut toml::Table, segments: &[&Segment]) -> bool {
    let mut keys = Vec::with_capacity(segments.len());
    for segment in segments {
        match segment {
            Segment::Map { key } => keys.push(key.clone()),
            Segment::Seq { .. } | Segment::Enum { .. } | Segment::Unknown => return false,
        }
    }
    let Some((last, parents)) = keys.split_last() else {
        return false;
    };

    let mut cursor = table;
    for key in parents {
        match cursor.get_mut(key) {
            Some(toml::Value::Table(inner)) => cursor = inner,
            _ => return false,
        }
    }
    cursor.remove(last).is_some()
}

/// Resolve which layer introduced `key`.
///
/// Provenance records *leaf* paths, so an unknown scalar hits directly. An
/// unknown table has no leaf entry of its own, so fall back to the first leaf
/// recorded beneath it — every leaf under an unknown table is equally
/// unknown, and the layer that set the first one is the file to open.
fn attribute(key: &str, provenance: &crate::ConfigProvenance) -> Option<LayerSource> {
    let index = provenance.keys.get(key).map_or_else(
        || {
            let prefix = format!("{key}.");
            provenance
                .keys
                .iter()
                .find(|(path, _)| path.starts_with(&prefix))
                .map(|(_, origin)| origin.layer)
        },
        |origin| Some(origin.layer),
    )?;
    provenance.layers.get(index).cloned()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn run(input: &str) -> CheckReport {
        check(input, &PathBuf::from("/nonexistent/config.toml")).expect("check runs")
    }

    fn paths(report: &CheckReport) -> Vec<&str> {
        report.findings.iter().map(|f| f.path.as_str()).collect()
    }

    /// The shipped defaults must be clean against their own schema. If this
    /// fails, `default.toml` grew a key the struct does not read — which
    /// would make every user's `config check` report a phux bug as their typo.
    #[test]
    fn the_embedded_defaults_check_clean() {
        let report = run("");
        assert!(
            report.is_ok(),
            "default.toml drifted: {:?}",
            report.findings
        );
    }

    /// The whole point: the parent table is in the path. serde alone says
    /// `enabledd`, which is ambiguous across the several tables that have an
    /// `enabled`.
    #[test]
    fn an_unknown_key_carries_its_parent_table() {
        let report = run("[sidebar]\nenabledd = true\n");
        assert_eq!(paths(&report), vec!["sidebar.enabledd"]);
        assert_eq!(report.findings[0].fault, Fault::UnknownKey);
    }

    /// serde's own text is preserved, because the `expected one of ...` list
    /// is what turns a rejection into a fix.
    #[test]
    fn the_finding_keeps_serdes_suggestion_list() {
        let report = run("[sidebar]\nenabledd = true\n");
        assert!(
            report.findings[0].message.contains("expected one of"),
            "lost the suggestion list: {}",
            report.findings[0].message
        );
    }

    /// Every typo in one pass, not one per run. Fixing a four-typo config
    /// should not take four edit-run cycles.
    #[test]
    fn every_unknown_key_is_reported_in_one_pass() {
        let report =
            run("[sidebar]\nenabledd = true\nwidht = 4\n\n[keybindings]\nwich-key = true\n");
        let found = paths(&report);
        for want in ["sidebar.enabledd", "sidebar.widht", "keybindings.wich-key"] {
            assert!(found.contains(&want), "missing {want} in {found:?}");
        }
        assert!(!report.truncated);
    }

    /// A wrong value is a different mistake from an unknown key, and is
    /// classified as such — they have different fixes.
    #[test]
    fn a_wrong_type_is_reported_as_a_bad_value() {
        let report = run("[keybindings]\nwhich-key = \"yes\"\n");
        assert_eq!(paths(&report), vec!["keybindings.which-key"]);
        assert_eq!(report.findings[0].fault, Fault::BadValue);
    }

    /// A typo and a bad value in the same file both surface. Stopping at the
    /// first would hide the other.
    #[test]
    fn a_typo_and_a_bad_value_are_both_reported() {
        let report = run("[keybindings]\nwhich-key = \"yes\"\nwich-key = true\n");
        let found = paths(&report);
        assert!(found.contains(&"keybindings.which-key"), "{found:?}");
        assert!(found.contains(&"keybindings.wich-key"), "{found:?}");
    }

    /// A correct config stays silent. A checker that cries wolf on a valid
    /// file is worse than no checker.
    #[test]
    fn a_valid_config_reports_nothing() {
        let report = run("[keybindings]\nwhich-key = false\n\n[sidebar]\nenabled = true\n");
        assert!(report.is_ok(), "false positives: {:?}", report.findings);
    }

    /// Free-form key spaces must not be flagged. `hooks` is a map keyed by
    /// arbitrary event names; treating those as unknown keys would make the
    /// checker unusable for anyone who uses hooks.
    #[test]
    fn free_form_map_keys_are_not_unknown_keys() {
        let report = run(
            "[[hooks.after-new-pane]]\nwhen = { cwd-startswith = \"/x\" }\naction = \"noop\"\n",
        );
        assert!(report.is_ok(), "hooks flagged: {:?}", report.findings);
    }

    /// The finding names the file to open, which is the real question once
    /// `extends` layers are in play.
    #[test]
    fn a_user_file_finding_is_attributed_to_that_file() {
        let path = Path::new("/nonexistent/config.toml");
        let report = check("[sidebar]\nenabledd = true\n", path).expect("check runs");
        assert_eq!(
            report.findings[0].source,
            Some(LayerSource::User(path.to_path_buf())),
            "origin was {}",
            report.findings[0].origin()
        );
    }

    /// TOML that does not parse is an error, not an empty report. "No
    /// findings" must never be said about a file that was never read.
    #[test]
    fn unparseable_toml_is_an_error_not_a_clean_report() {
        let err = check(
            "this is not = = toml\n",
            Path::new("/nonexistent/config.toml"),
        );
        assert!(err.is_err(), "malformed TOML reported as clean");
    }
}
