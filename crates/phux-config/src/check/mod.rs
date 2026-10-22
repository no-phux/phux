//! Config validation that says *where* the problem is.
//!
//! `deny_unknown_fields` already rejects typos, but serde names only the leaf
//! field, a merged layer stack has no usable span, and the loader stops at the
//! first error. [`check`] reports every finding in one pass with its full
//! dotted path (from [`serde_path_to_error`]) and the layer that introduced it
//! (ADR-0039).
//!
//! A semantic pass then runs over the deserialized [`Config`] for mistakes
//! that parse fine and do nothing at runtime: out-of-range ceilings, chords
//! that do not parse, unknown action / hook / widget / theme-slot names,
//! theme colours that do not parse, and bindings that shadow each other.

mod semantic;

use std::path::Path;

use serde_path_to_error::Segment;

use crate::{
    Config, ConfigError, ConfigProvenance, LayerSource, merged_config_with_provenance, vocab,
};
use semantic::semantic_pass;

/// Upper bound on findings in one run: each finding costs a full re-walk of
/// the merged table. Reaching the cap is reported, never silent.
const MAX_FINDINGS: usize = 64;

/// What kind of mistake a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The schema has no such key.
    UnknownKey,
    /// The key exists but the value has the wrong type, shape, or range.
    BadValue,
    /// A chord that does not parse, or a binding another one shadows.
    BadChord,
    /// A name outside the validation vocabulary ([`crate::vocab`],
    /// [`crate::theme_slots`]): an action, hook event, `when` key, widget
    /// kind, or `[theme]` slot that does nothing.
    UnknownName,
    /// A hook action that can never execute server-side (only `run` does);
    /// a match still consumes the event under first-match-wins.
    DeadAction,
}

impl Fault {
    /// Short label for human output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UnknownKey => "unknown key",
            Self::BadValue => "bad value",
            Self::BadChord => "bad chord",
            Self::UnknownName => "unknown name",
            Self::DeadAction => "dead action",
        }
    }
}

/// One problem found in the resolved config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Dotted path to the key, as TOML addresses it (e.g. `sidebar.enabledd`).
    pub path: String,
    /// What kind of mistake it is.
    pub fault: Fault,
    /// Human-readable detail (serde's own message for schema faults).
    pub message: String,
    /// The layer that introduced the key, when it can be attributed.
    pub source: Option<LayerSource>,
}

impl Finding {
    /// Human-readable origin: the layer's file, or a stable label.
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
    /// Whether the finding cap was reached, so the list is partial.
    pub truncated: bool,
}

impl CheckReport {
    /// Whether the config is clean.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Check a config's whole resolved layer stack. `path` is used for errors and
/// as the base directory for relative `extends` entries.
///
/// # Errors
///
/// [`ConfigError`] only when the check cannot run at all (unparseable TOML, an
/// unreadable or cyclic layer): a file never read is never reported clean.
pub fn check(user_input: &str, path: &Path) -> Result<CheckReport, ConfigError> {
    let (mut merged, provenance) = merged_config_with_provenance(user_input, path)?;
    let mut findings = Vec::new();
    let mut truncated = false;

    // Schema pass: peel one offending key per round until the table
    // deserializes; the result feeds the semantic pass.
    let config = loop {
        let attempt: Result<Config, _> =
            serde_path_to_error::deserialize(toml::Value::Table(merged.clone()));
        let err = match attempt {
            Ok(config) => break Some(config),
            Err(err) => err,
        };

        let segments: Vec<&Segment> = err.path().iter().collect();
        // serde appends an "in `<table>`" line that `path` already says.
        let raw = err.inner().to_string();
        let message = raw.lines().next().unwrap_or(&raw).to_owned();
        let fault = if message.starts_with("unknown field") {
            Fault::UnknownKey
        } else {
            Fault::BadValue
        };
        let key_path = err.path().to_string();

        // An unremovable finding would be rediscovered forever: stop instead.
        if !remove_at(&mut merged, &segments) {
            findings.push(Finding {
                path: key_path,
                fault,
                message,
                source: None,
            });
            break None;
        }
        push_semantic(&mut findings, &provenance, key_path, fault, message);
        if findings.len() >= MAX_FINDINGS {
            truncated = true;
            break None;
        }
    };

    if let Some(config) = config {
        semantic_pass(&config, &provenance, &mut findings);
        if findings.len() > MAX_FINDINGS {
            findings.truncate(MAX_FINDINGS);
            truncated = true;
        }
    }

    Ok(CheckReport {
        findings,
        truncated,
    })
}

/// `" (did you mean `x`?)"`, or empty when nothing is close.
fn suggest(name: &str, vocabulary: &[&str]) -> String {
    vocab::did_you_mean(name, vocabulary)
        .map(|hit| format!(" (did you mean `{hit}`?)"))
        .unwrap_or_default()
}

/// The layer that contributed element `index` of the array at `table_path`;
/// with `-append` layering, elements of one array can come from different
/// files. Falls back to the array's own layer.
fn array_entry_source(
    provenance: &ConfigProvenance,
    table_path: &str,
    index: usize,
) -> Option<LayerSource> {
    let origin = provenance.keys.get(table_path)?;
    let layer = origin
        .elements
        .as_ref()
        .and_then(|elements| elements.get(index))
        .copied()
        .unwrap_or(origin.layer);
    provenance.layers.get(layer).cloned()
}

/// Record one finding, attributed to the layer that set the key.
fn push_semantic(
    findings: &mut Vec<Finding>,
    provenance: &ConfigProvenance,
    path: String,
    fault: Fault,
    message: String,
) {
    let source = attribute(&path, provenance);
    findings.push(Finding {
        path,
        fault,
        message,
        source,
    });
}

/// Remove the value addressed by `segments`; returns whether anything was
/// removed. Array elements cannot be excised without renumbering, so a
/// non-map segment stops the walk.
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

/// Which layer introduced `key`. An unknown table has no leaf entry of its
/// own, so fall back to the first leaf recorded beneath it.
fn attribute(key: &str, provenance: &ConfigProvenance) -> Option<LayerSource> {
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
mod tests;
