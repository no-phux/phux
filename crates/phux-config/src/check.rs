//! Config validation that says *where* the problem is.
//!
//! [`crate::Config`] already carries `#[serde(deny_unknown_fields)]`, so a
//! typo is rejected rather than silently ignored. What it is not is
//! *locatable*. serde names only the leaf field:
//!
//! ```text
//! config.toml: unknown field `enabledd`, expected one of `enabled`, `width`, `position`
//! ```
//!
//! Three things are wrong with that, and this module fixes all three.
//!
//! 1. **No parent path.** `enabledd` does not say which table it is in, and
//!    several tables share key names (`enabled`, `width`, `position`). This
//!    module reports `sidebar.enabledd`, derived from the schema walk by
//!    [`serde_path_to_error`] — so it cannot drift the way a hand-maintained
//!    key list would.
//! 2. **No usable position.** A deserialize error on the merged layer stack
//!    carries no span, because the value being deserialized is not the
//!    user's text. (The loader used to fabricate a `1:1` here; it now
//!    reports no position at all — phux-i0e8.3.5 — since a confident, wrong
//!    line number is worse than none.)
//!    This module attributes each finding to the layer that introduced it
//!    (ADR-0039) instead, which with `extends` in play is the question the
//!    operator actually has: is the typo mine, or the distro's?
//! 3. **One at a time.** The loader stops at the first bad key, so fixing a
//!    config with four typos takes four edit-run cycles. This module removes
//!    each offending key and re-walks, collecting every finding in one pass.
//!
//! Beyond the schema walk, a **semantic pass** (phux-i0e8.3.2) runs over the
//! merged-and-pruned [`Config`] once it deserializes: every keybinding chord
//! must parse under the [`crate::keybind`] grammar, every action name must
//! exist in [`crate::vocab::ACTION_NAMES`] (unknown names carry a
//! did-you-mean suggestion), and the bindings as a set must build an
//! unambiguous resolver. These are exactly the mistakes that parse fine and
//! then do nothing at runtime — the silent-no-op traps the schema walk
//! cannot see because chords are free-form `BTreeMap` string keys and
//! actions are free-form strings.

use std::collections::BTreeMap;
use std::path::Path;

use serde_path_to_error::Segment;

use crate::keybind::{KeybindError, Resolver, parse_chord, parse_chord_sequence};
use crate::{
    Action, Config, ConfigError, HookEntry, KeybindingsCfg, LayerSource,
    merged_config_with_provenance, vocab,
};

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
    /// A keybinding chord that does not parse under the chord grammar, or
    /// that clashes with another binding (one binding's sequence is a
    /// strict prefix of another's, so the longer could never fire).
    BadChord,
    /// A name outside the validation vocabulary ([`crate::vocab`]) — an
    /// action name no dispatcher arm handles, a hook event the server
    /// never fires, or a hook `when` key outside the event's context —
    /// so the entry would parse, load, and then do nothing.
    UnknownName,
    /// A hook action that can never execute server-side (only `run`
    /// does; `noop` is the deliberate sentinel and is not flagged). A
    /// matched entry still consumes its event under first-match-wins,
    /// so a dead action can also shadow a live entry below it.
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

    // Schema pass: peel unknown keys and bad values one at a time until the
    // merged table deserializes. The successful deserialization is kept —
    // it is the merged-and-pruned [`Config`] the semantic pass inspects.
    let config = loop {
        // Deserializing a clone each round is the price of collecting more
        // than one finding: the walk consumes the value, and the offending
        // key has to be removed from the original before the next attempt.
        let attempt: Result<Config, _> =
            serde_path_to_error::deserialize(toml::Value::Table(merged.clone()));
        let err = match attempt {
            Ok(config) => break Some(config),
            Err(err) => err,
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
            break None;
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
            break None;
        }
    };

    // Semantic pass: only meaningful over a config that actually
    // deserialized. When the schema walk gave up early (unremovable
    // finding, cap reached) the report is already loud and partial, and
    // says so.
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

/// Validators that need the typed, merged-and-pruned [`Config`] rather
/// than the raw TOML table: keybindings (phux-i0e8.3.2) and hooks
/// (phux-i0e8.3.3).
fn semantic_pass(
    config: &Config,
    provenance: &crate::ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    keybinding_findings(&config.keybindings, provenance, findings);
    hook_findings(&config.hooks, provenance, findings);
}

/// Semantic keybinding validation (phux-i0e8.3.2), catching the two
/// silent-no-op traps the schema walk cannot see:
///
/// 1. Chord strings are free-form `BTreeMap` keys, so a malformed chord
///    parses as TOML and only fails at resolver-build time — where, before
///    the lenient build, it silently disabled *every* binding.
/// 2. Action names are free-form strings ([`Action::Bare`] accepts
///    anything), so a typo'd action binds a key to nothing, logged at
///    debug level only.
///
/// Parameters of [`Action::Parameterized`] are deliberately not validated
/// here — per-action argument schemas live in the dispatcher, not the
/// loader (see `crate::schema::ParamAction`); only the action *name* is
/// checked. Cross-binding faults (ambiguous prefixes) come from the
/// lenient resolver build, filtered to [`KeybindError::AmbiguousPrefix`]
/// because per-binding parse errors were already reported with better
/// paths above.
fn keybinding_findings(
    kb: &KeybindingsCfg,
    provenance: &crate::ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    if let Err(error) = parse_chord(&kb.prefix) {
        push_semantic(
            findings,
            provenance,
            "keybindings.prefix".to_owned(),
            Fault::BadChord,
            error.to_string(),
        );
    }

    for (table, bindings) in [("global", &kb.global), ("prefix-table", &kb.prefix_table)] {
        for (binding, action) in bindings {
            let path = binding_path(table, binding);
            if let Err(error) = parse_chord_sequence(binding) {
                push_semantic(
                    findings,
                    provenance,
                    path.clone(),
                    Fault::BadChord,
                    error.to_string(),
                );
            }
            let name = action_name(action);
            if !vocab::ACTION_NAMES.contains(&name) {
                let message = vocab::did_you_mean(name, vocab::ACTION_NAMES).map_or_else(
                    || format!("unknown action `{name}`"),
                    |suggestion| format!("unknown action `{name}` (did you mean `{suggestion}`?)"),
                );
                push_semantic(findings, provenance, path, Fault::UnknownName, message);
            }
        }
    }

    // Cross-binding faults: build the resolver leniently and keep only the
    // ambiguity diagnostics — syntax errors were reported per binding above,
    // and reporting them twice would double-count every bad chord.
    let (_, diagnostics) = Resolver::new_lenient(kb);
    for diagnostic in diagnostics {
        if matches!(diagnostic.error, KeybindError::AmbiguousPrefix(_)) {
            push_semantic(
                findings,
                provenance,
                ambiguous_binding_path(kb, &diagnostic.binding),
                Fault::BadChord,
                diagnostic.error.to_string(),
            );
        }
    }
}

/// Semantic hook validation (phux-i0e8.3.3). Hooks fail open three
/// ways, and each is a silent no-op at runtime the schema walk cannot
/// see (`[[hooks.<name>]]` is a free-form map):
///
/// 1. An event name outside [`vocab::HOOK_EVENTS`] never fires — the
///    whole table is dead, so one finding covers it and its entries are
///    not validated further.
/// 2. A `when` key outside the event's context
///    ([`vocab::hook_context_keys`]) never matches. The `-startswith`
///    suffix strips off before the lookup, mirroring the dispatcher's
///    clause evaluation.
/// 3. An action that never executes server-side
///    ([`vocab::hook_action_is_executable`]) still consumes the event
///    under first-match-wins. `noop` is exempt: consuming an event and
///    doing nothing is that sentinel's documented job.
///
/// The server logs the same findings at startup
/// (`HookCatalog::from_config` in phux-server) so a config that skipped
/// `phux config check` is still loud.
fn hook_findings(
    hooks: &BTreeMap<String, Vec<HookEntry>>,
    provenance: &crate::ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    for (event, entries) in hooks {
        let table_path = crate::layer::child_path("hooks", event);
        let Some(context_keys) = vocab::hook_context_keys(event) else {
            let message = vocab::did_you_mean(event, vocab::HOOK_EVENTS).map_or_else(
                || format!("unknown hook event `{event}`; these entries will never fire"),
                |suggestion| {
                    format!(
                        "unknown hook event `{event}` (did you mean `{suggestion}`?); \
                         these entries will never fire"
                    )
                },
            );
            push_semantic(
                findings,
                provenance,
                table_path,
                Fault::UnknownName,
                message,
            );
            continue;
        };

        for (index, entry) in entries.iter().enumerate() {
            let entry_path = format!("{table_path}[{index}]");
            let source = hook_entry_source(provenance, &table_path, index);

            for key in entry.when.keys() {
                let base = key.strip_suffix("-startswith").unwrap_or(key);
                if context_keys.contains(&base) {
                    continue;
                }
                let suggestion = vocab::did_you_mean(base, context_keys)
                    .map(|hit| format!(" (did you mean `{hit}`?)"))
                    .unwrap_or_default();
                findings.push(Finding {
                    path: crate::layer::child_path(&format!("{entry_path}.when"), key),
                    fault: Fault::UnknownName,
                    message: format!(
                        "unknown when key `{key}`: this clause can never match; \
                         `{event}` context keys are {}{suggestion}",
                        context_keys.join(", "),
                    ),
                    source: source.clone(),
                });
            }

            let name = action_name(&entry.action);
            if name != "noop" && !vocab::hook_action_is_executable(&entry.action) {
                let message = if name == "run" {
                    "`run` action has no usable `command` (need a non-blank string or a \
                     non-empty array of strings); a match consumes the event and runs nothing"
                        .to_owned()
                } else {
                    format!(
                        "action `{name}` never executes server-side (only `run` does; \
                         `noop` is the deliberate no-op); a match still consumes the event"
                    )
                };
                findings.push(Finding {
                    path: format!("{entry_path}.action"),
                    fault: Fault::DeadAction,
                    message,
                    source: source.clone(),
                });
            }
        }
    }
}

/// Resolve the layer that contributed hook entry `index` of the array at
/// `table_path`.
///
/// Hook entries live in an array-of-tables, and provenance records
/// arrays as a single leaf with per-element contributor indices
/// ([`crate::KeyOrigin::elements`]) — with `-append` layering, entry 0
/// and entry 1 of the same event can come from different files. Falls
/// back to the array's own layer when the element index is out of range.
fn hook_entry_source(
    provenance: &crate::ConfigProvenance,
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

/// Record one semantic finding, attributing it to the layer that set the
/// key (same provenance lookup the schema walk uses).
fn push_semantic(
    findings: &mut Vec<Finding>,
    provenance: &crate::ConfigProvenance,
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

/// The action name a binding invokes, whichever spelling it used.
fn action_name(action: &Action) -> &str {
    match action {
        Action::Bare(name) => name,
        Action::Parameterized(parameterized) => &parameterized.action,
    }
}

/// Dotted path for a binding key in `[keybindings.<table>]`, quoted
/// exactly the way provenance records it (chord keys are rarely bare:
/// `keybindings.prefix-table."q-"`).
fn binding_path(table: &str, binding: &str) -> String {
    crate::layer::child_path(&format!("keybindings.{table}"), binding)
}

/// Locate an [`KeybindError::AmbiguousPrefix`] diagnostic's binding text.
///
/// [`crate::keybind::BindingDiagnostic`] carries the binding as written
/// but not which table it came from, so membership decides: the
/// prefix-chord-conflict case reports the *global* binding that could
/// never fire, so `global` is checked first; a diagnostic matching
/// neither table carries the prefix string itself.
fn ambiguous_binding_path(kb: &KeybindingsCfg, binding: &str) -> String {
    if kb.global.contains_key(binding) {
        binding_path("global", binding)
    } else if kb.prefix_table.contains_key(binding) {
        binding_path("prefix-table", binding)
    } else {
        "keybindings.prefix".to_owned()
    }
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

    /// Free-form key spaces must not be flagged by the *schema* walk.
    /// `hooks` is a map keyed by arbitrary event names; treating those as
    /// unknown keys would make the checker unusable for anyone who uses
    /// hooks. (The semantic pass has its own, vocabulary-aware opinion —
    /// see the hook tests below — so this fixture uses a valid entry.)
    #[test]
    fn free_form_map_keys_are_not_unknown_keys() {
        let report = run(
            "[[hooks.after-new-pane]]\nwhen = { session-startswith = \"work\" }\naction = \"noop\"\n",
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

    // -- semantic pass: keybindings (phux-i0e8.3.2) ------------------------

    /// The umbrella bead's motivating trap: `"kill-pain"` parses, loads,
    /// and the key silently does nothing. The check must name the binding
    /// and suggest the fix.
    #[test]
    fn a_typoed_action_is_flagged_with_a_suggestion() {
        let report = run("[keybindings.prefix-table]\nq = \"kill-pain\"\n");
        assert_eq!(paths(&report), vec!["keybindings.prefix-table.q"]);
        let finding = &report.findings[0];
        assert_eq!(finding.fault, Fault::UnknownName);
        assert!(
            finding
                .message
                .contains("unknown action `kill-pain` (did you mean `kill-pane`?)"),
            "no suggestion in: {}",
            finding.message
        );
        // Semantic findings carry layer attribution like schema findings do.
        assert_eq!(
            finding.source,
            Some(LayerSource::User(PathBuf::from("/nonexistent/config.toml"))),
            "origin was {}",
            finding.origin()
        );
    }

    /// A malformed chord key is valid TOML (chords are free-form map keys),
    /// so only the semantic pass can catch it. Before the lenient resolver
    /// this single typo silently disabled every binding including detach.
    #[test]
    fn a_malformed_chord_key_is_flagged_as_a_bad_chord() {
        let report = run("[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n");
        // `-` is a bare TOML key character, so the path needs no quoting.
        assert_eq!(paths(&report), vec!["keybindings.prefix-table.q-"]);
        assert_eq!(report.findings[0].fault, Fault::BadChord);
    }

    /// A prefix that does not parse is located at its own key, not blamed
    /// on the bindings that hang off it.
    #[test]
    fn a_malformed_prefix_is_flagged_at_its_own_path() {
        let report = run("[keybindings]\nprefix = \"Ctrl-a\"\n");
        assert_eq!(paths(&report), vec!["keybindings.prefix"]);
        assert_eq!(report.findings[0].fault, Fault::BadChord);
    }

    /// A valid keybindings table — chords parse, actions exist — is clean.
    /// The semantic pass must not cry wolf on a working config.
    #[test]
    fn valid_keybindings_check_clean() {
        let report = run(
            "[keybindings]\nprefix = \"C-b\"\n\n[keybindings.global]\n\"M-Enter\" = \"detach\"\n\n[keybindings.prefix-table]\nw = \"window-picker\"\n",
        );
        assert!(report.is_ok(), "false positives: {:?}", report.findings);
    }

    /// A parameterized action with a valid name is clean: the args are
    /// deliberately not validated here — per-action argument schemas live
    /// in the dispatcher, not the loader.
    #[test]
    fn a_parameterized_action_with_a_valid_name_is_clean() {
        let report = run(
            "[keybindings.prefix-table]\nR = { action = \"resize-pane\", direction = \"left\", amount = 3, made-up-arg = true }\n",
        );
        assert!(report.is_ok(), "false positives: {:?}", report.findings);
    }

    /// A parameterized action's *name* is still validated, suggestion and
    /// all — only its arguments get a pass.
    #[test]
    fn a_parameterized_action_with_a_typoed_name_is_flagged() {
        let report = run(
            "[keybindings.prefix-table]\nR = { action = \"resize-pain\", direction = \"left\" }\n",
        );
        assert_eq!(paths(&report), vec!["keybindings.prefix-table.R"]);
        assert_eq!(report.findings[0].fault, Fault::UnknownName);
        assert!(
            report.findings[0].message.contains("`resize-pane`"),
            "no suggestion in: {}",
            report.findings[0].message
        );
    }

    /// One binding's sequence shadowing another's is a cross-binding fault
    /// only the resolver build can see; the check reports it against the
    /// binding that loses.
    #[test]
    fn an_ambiguous_prefix_pair_is_flagged() {
        let report =
            run("[keybindings.prefix-table]\n\"y\" = \"copy-mode\"\n\"y x\" = \"kill-pane\"\n");
        assert_eq!(paths(&report), vec!["keybindings.prefix-table.\"y x\""]);
        let finding = &report.findings[0];
        assert_eq!(finding.fault, Fault::BadChord);
        assert!(
            finding.message.contains("ambiguous prefix"),
            "wrong message: {}",
            finding.message
        );
    }

    // -- semantic pass: hooks (phux-i0e8.3.3) ------------------------------

    /// `[[hooks.pane-exited]]` (real event: `pane-exit`) parses fine and
    /// never fires. The check must flag the event name with a suggestion,
    /// and must not pile per-entry findings onto an already-dead table.
    #[test]
    fn an_unknown_hook_event_is_flagged_with_a_suggestion() {
        let report = run("[[hooks.pane-exited]]\naction = \"noop\"\n");
        assert_eq!(paths(&report), vec!["hooks.pane-exited"]);
        let finding = &report.findings[0];
        assert_eq!(finding.fault, Fault::UnknownName);
        assert!(
            finding
                .message
                .contains("unknown hook event `pane-exited` (did you mean `pane-exit`?)"),
            "no suggestion in: {}",
            finding.message
        );
    }

    /// A `when` key outside the event's context can never match, so the
    /// entry silently never fires. The finding names the entry, the key,
    /// and the keys that would work.
    #[test]
    fn an_unknown_when_key_is_flagged_with_the_valid_keys() {
        let report = run(
            "[[hooks.pane-exit]]\nwhen = { exitcode = 0 }\naction = { kind = \"run\", command = \"true\" }\n",
        );
        assert_eq!(paths(&report), vec!["hooks.pane-exit[0].when.exitcode"]);
        let finding = &report.findings[0];
        assert_eq!(finding.fault, Fault::UnknownName);
        assert!(
            finding
                .message
                .contains("context keys are exit-code, terminal-id"),
            "valid keys missing from: {}",
            finding.message
        );
        assert!(
            finding.message.contains("did you mean `exit-code`?"),
            "no suggestion in: {}",
            finding.message
        );
    }

    /// The `-startswith` suffix is match grammar, not part of the key: it
    /// strips before validation, so a valid base passes and an invalid
    /// base is flagged under the full spelling the user wrote.
    #[test]
    fn startswith_strips_before_the_context_key_lookup() {
        let clean = run(
            "[[hooks.after-new-pane]]\nwhen = { session-startswith = \"work\" }\naction = \"noop\"\n",
        );
        assert!(clean.is_ok(), "false positives: {:?}", clean.findings);

        let report = run(
            "[[hooks.after-new-pane]]\nwhen = { cwd-startswith = \"/x\" }\naction = \"noop\"\n",
        );
        assert_eq!(
            paths(&report),
            vec!["hooks.after-new-pane[0].when.cwd-startswith"]
        );
        assert_eq!(report.findings[0].fault, Fault::UnknownName);
        assert!(
            report.findings[0]
                .message
                .contains("unknown when key `cwd-startswith`"),
            "wrong message: {}",
            report.findings[0].message
        );
    }

    /// A matched entry whose action never executes server-side consumes
    /// the event and runs nothing — flagged per entry, with the index, so
    /// the second entry of the same event is named precisely.
    #[test]
    fn a_never_executable_hook_action_is_flagged_per_entry() {
        let report = run(
            "[[hooks.pane-exit]]\naction = { kind = \"run\", command = \"true\" }\n\n\
             [[hooks.pane-exit]]\naction = { kind = \"message\", text = \"bye\" }\n",
        );
        assert_eq!(paths(&report), vec!["hooks.pane-exit[1].action"]);
        let finding = &report.findings[0];
        assert_eq!(finding.fault, Fault::DeadAction);
        assert!(
            finding
                .message
                .contains("action `message` never executes server-side"),
            "wrong message: {}",
            finding.message
        );
    }

    /// A `run` action without a usable `command` is just as dead as a
    /// client-side kind, and gets a fix-oriented message.
    #[test]
    fn a_run_action_without_a_usable_command_is_flagged() {
        let report = run("[[hooks.pane-exit]]\naction = { kind = \"run\", command = [] }\n");
        assert_eq!(paths(&report), vec!["hooks.pane-exit[0].action"]);
        assert_eq!(report.findings[0].fault, Fault::DeadAction);
        assert!(
            report.findings[0].message.contains("no usable `command`"),
            "wrong message: {}",
            report.findings[0].message
        );
    }

    /// `noop` is the documented consume-and-do-nothing sentinel, not a
    /// mistake; a valid hooks table stays clean.
    #[test]
    fn valid_hooks_including_noop_check_clean() {
        let report = run(
            "[[hooks.pane-exit]]\nwhen = { exit-code = 0 }\naction = \"noop\"\n\n\
             [[hooks.pane-exit]]\nwhen = { exit-code = \"*\" }\naction = { kind = \"run\", command = \"say done\" }\n\n\
             [[hooks.agent-state-changed]]\nwhen = { to = \"blocked\" }\naction = { kind = \"run\", command = [\"afplay\", \"/tmp/x.aiff\"] }\n",
        );
        assert!(report.is_ok(), "false positives: {:?}", report.findings);
    }

    /// Hook findings carry layer attribution like every other finding:
    /// the array element's contributing layer, which is the file to open.
    #[test]
    fn a_hook_finding_is_attributed_to_the_user_file() {
        let path = Path::new("/nonexistent/config.toml");
        let report = check(
            "[[hooks.pane-exit]]\naction = { kind = \"message\", text = \"bye\" }\n",
            path,
        )
        .expect("check runs");
        assert_eq!(
            report.findings[0].source,
            Some(LayerSource::User(path.to_path_buf())),
            "origin was {}",
            report.findings[0].origin()
        );
    }
}
