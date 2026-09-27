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
//! that do not parse, unknown action / hook / widget names, and bindings that
//! shadow each other.

use std::collections::BTreeMap;
use std::path::Path;

use serde_path_to_error::Segment;

use crate::keybind::{KeybindError, Resolver, parse_chord, parse_chord_sequence};
use crate::schema::{Widget, WidgetSpec};
use crate::widget::{WidgetError, WidgetRegistry};
use crate::{
    Action, Config, ConfigError, ConfigProvenance, DefaultsCfg, HookEntry, KeybindingsCfg,
    LayerSource, merged_config_with_provenance, vocab,
};

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
    /// A name outside the validation vocabulary ([`crate::vocab`]): an
    /// action, hook event, `when` key, or widget kind that does nothing.
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

fn semantic_pass(config: &Config, provenance: &ConfigProvenance, findings: &mut Vec<Finding>) {
    defaults_findings(&config.defaults, provenance, findings);
    limits_findings(&config.limits, provenance, findings);
    keybinding_findings(&config.keybindings, provenance, findings);
    hook_findings(&config.hooks, provenance, findings);
    status_widget_findings(&config.status, provenance, findings);
}

/// `[defaults]` values that parse as any `u32` but have a ceiling. Flagged
/// rather than clamped silently, so the operator learns what they asked for.
fn defaults_findings(
    defaults: &DefaultsCfg,
    provenance: &ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    let ceilings: [(&str, u32, u32, &str); 7] = [
        (
            "history-bytes",
            defaults.history_bytes,
            crate::MAX_HISTORY_BYTES,
            " bytes (64 MiB); retained history is held resident per pane for the life of \
             the session, so a larger value is memory the server will not spend",
        ),
        (
            "agent-log-bytes",
            defaults.agent_log_bytes,
            crate::MAX_AGENT_LOG_BYTES,
            " bytes (64 MiB); a session's retained records are replayed in full on every \
             attach to its stream, so a larger value buys a transcript nobody waits out",
        ),
        (
            "event-journal-entries",
            defaults.event_journal_entries,
            crate::MAX_EVENT_JOURNAL_ENTRIES,
            " events; the journal is held resident for the life of the server",
        ),
        (
            "event-journal-bytes",
            defaults.event_journal_bytes,
            crate::MAX_EVENT_JOURNAL_BYTES,
            " bytes (64 MiB); the journal is held resident for the life of the server",
        ),
        (
            "approval-max-pending",
            defaults.approval_max_pending,
            crate::MAX_APPROVAL_MAX_PENDING,
            "; each pending approval keeps its command and a record in the server's metadata",
        ),
        (
            "approval-max-pending-total",
            defaults.approval_max_pending_total,
            crate::MAX_APPROVAL_MAX_PENDING_TOTAL,
            "",
        ),
        (
            "retain-on-exit-max",
            defaults.retain_on_exit_max,
            crate::MAX_RETAIN_ON_EXIT_MAX,
            " retained panes; each one holds its grid and history until it is purged",
        ),
    ];
    for (key, value, max, why) in ceilings {
        if value > max {
            push_semantic(
                findings,
                provenance,
                format!("defaults.{key}"),
                Fault::BadValue,
                format!("{value} exceeds the accepted maximum of {max}{why}"),
            );
        }
    }

    // A zero TTL would expire every hold before anyone could see it (ADR-0128).
    let ttl = defaults.approval_ttl_secs;
    if ttl == 0 || ttl > crate::MAX_APPROVAL_TTL_SECS {
        push_semantic(
            findings,
            provenance,
            "defaults.approval-ttl-secs".to_owned(),
            Fault::BadValue,
            format!(
                "{ttl} is outside 1..={}: a held action must stay visible long enough to be \
                 decided, and an approval is never a standing grant",
                crate::MAX_APPROVAL_TTL_SECS,
            ),
        );
    }
}

/// `limits.metadata-value-bytes` below the server's own agent-session record
/// size would break session metadata writes; the server clamps it up at
/// startup, so a smaller value is not actually in force.
fn limits_findings(
    limits: &crate::LimitsCfg,
    provenance: &ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    let floor = u32::try_from(phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES)
        .unwrap_or(u32::MAX);
    if limits.metadata_value_bytes < floor {
        push_semantic(
            findings,
            provenance,
            "limits.metadata-value-bytes".to_owned(),
            Fault::BadValue,
            format!(
                "{} is below the built-in floor of {floor} bytes (the size of the server's own \
                 agent-session record write, checked against this same cap); the server clamps \
                 it up to {floor} at startup rather than refuse to start, so this value is not \
                 actually in force",
                limits.metadata_value_bytes,
            ),
        );
    }
}

/// Chords must parse and action names must exist. Parameterized-action
/// arguments are validated by the dispatcher, not here.
fn keybinding_findings(
    kb: &KeybindingsCfg,
    provenance: &ConfigProvenance,
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
                let message = format!(
                    "unknown action `{name}`{}",
                    suggest(name, vocab::ACTION_NAMES)
                );
                push_semantic(findings, provenance, path, Fault::UnknownName, message);
            }
        }
    }

    // Only ambiguity: per-binding syntax errors were reported above.
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

/// Hooks fail open three ways: an unknown event never fires (one finding for
/// the whole table), a `when` key outside the event's context never matches,
/// and an action other than `run` / `noop` consumes the event and does
/// nothing. The server logs the same findings at startup.
fn hook_findings(
    hooks: &BTreeMap<String, Vec<HookEntry>>,
    provenance: &ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    for (event, entries) in hooks {
        let table_path = crate::layer::child_path("hooks", event);
        let Some(context_keys) = vocab::hook_context_keys(event) else {
            let message = format!(
                "unknown hook event `{event}`{}; these entries will never fire",
                suggest(event, vocab::HOOK_EVENTS)
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
            let source = array_entry_source(provenance, &table_path, index);

            for key in entry.when.keys() {
                let base = key.strip_suffix("-startswith").unwrap_or(key);
                if context_keys.contains(&base) {
                    continue;
                }
                findings.push(Finding {
                    path: crate::layer::child_path(&format!("{entry_path}.when"), key),
                    fault: Fault::UnknownName,
                    message: format!(
                        "unknown when key `{key}`: this clause can never match; \
                         `{event}` context keys are {}{}",
                        context_keys.join(", "),
                        suggest(base, context_keys),
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

/// Build every status-bar widget exactly as the TUI will, one at a time so
/// every bad widget is reported in one pass.
fn status_widget_findings(
    status: &crate::StatusCfg,
    provenance: &ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    let registry = WidgetRegistry::with_builtins();
    let kinds = registry.kinds();
    for (slot, widgets) in [
        ("left", &status.left),
        ("center", &status.center),
        ("right", &status.right),
    ] {
        let table_path = format!("status.{slot}");
        for (index, entry) in widgets.iter().enumerate() {
            let spec = match entry {
                Widget::Bare(kind) => WidgetSpec {
                    kind: kind.clone(),
                    opts: BTreeMap::new(),
                },
                Widget::Spec(spec) => spec.clone(),
            };
            let Err(error) = registry.build(&spec) else {
                continue;
            };
            let (fault, message) = match error {
                WidgetError::UnknownKind(kind) => (
                    Fault::UnknownName,
                    format!("unknown widget kind `{kind}`{}", suggest(&kind, &kinds)),
                ),
                invalid @ WidgetError::InvalidOption { .. } => {
                    (Fault::BadValue, invalid.to_string())
                }
            };
            findings.push(Finding {
                path: format!("{table_path}[{index}]"),
                fault,
                message,
                source: array_entry_source(provenance, &table_path, index),
            });
        }
    }
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

fn action_name(action: &Action) -> &str {
    match action {
        Action::Bare(name) => name,
        Action::Parameterized(parameterized) => &parameterized.action,
    }
}

/// Dotted path for a binding key, quoted the way provenance records it.
fn binding_path(table: &str, binding: &str) -> String {
    crate::layer::child_path(&format!("keybindings.{table}"), binding)
}

/// Locate an ambiguous-prefix diagnostic's binding. The prefix-conflict case
/// reports the global binding that can never fire, so `global` is checked
/// first; a match in neither table is the prefix itself.
fn ambiguous_binding_path(kb: &KeybindingsCfg, binding: &str) -> String {
    if kb.global.contains_key(binding) {
        binding_path("global", binding)
    } else if kb.prefix_table.contains_key(binding) {
        binding_path("prefix-table", binding)
    } else {
        "keybindings.prefix".to_owned()
    }
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
mod tests {
    use std::path::Path;

    use super::*;

    const PATH: &str = "/nonexistent/config.toml";

    fn run(input: &str) -> CheckReport {
        check(input, Path::new(PATH)).expect("check runs")
    }

    fn paths(report: &CheckReport) -> Vec<&str> {
        report.findings.iter().map(|f| f.path.as_str()).collect()
    }

    /// Configs that must check clean: the shipped defaults (otherwise every
    /// user's check reports a phux bug as their typo) and valid use of each
    /// free-form surface.
    #[test]
    fn valid_configs_check_clean() {
        let max_history = crate::MAX_HISTORY_BYTES;
        let floor = phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES;
        for input in [
            String::new(),
            "[keybindings]\nwhich-key = false\n\n[sidebar]\nenabled = true\n".to_owned(),
            format!("[defaults]\nhistory-bytes = {max_history}\n"),
            "[defaults]\nhistory-bytes = 1\n".to_owned(),
            format!("[limits]\nmetadata-value-bytes = {floor}\n"),
            "[[hooks.after-new-pane]]\nwhen = { session-startswith = \"work\" }\naction = \"noop\"\n".to_owned(),
            "[keybindings]\nprefix = \"C-b\"\n\n[keybindings.global]\n\"M-Enter\" = \"detach\"\n\n[keybindings.prefix-table]\nw = \"window-picker\"\n".to_owned(),
            // Parameterized-action arguments are the dispatcher's business.
            "[keybindings.prefix-table]\nR = { action = \"resize-pane\", direction = \"left\", amount = 3, made-up-arg = true }\n".to_owned(),
            "[[hooks.pane-exit]]\nwhen = { exit-code = 0 }\naction = \"noop\"\n\n\
             [[hooks.pane-exit]]\nwhen = { exit-code = \"*\" }\naction = { kind = \"run\", command = \"say done\" }\n\n\
             [[hooks.agent-state-changed]]\nwhen = { to = \"blocked\" }\naction = { kind = \"run\", command = [\"afplay\", \"/tmp/x.aiff\"] }\n".to_owned(),
            "[status]\nleft = [{ kind = \"windows\", separator = \" | \" }]\n\
             center = [\"help-hints\"]\n\
             right = [{ kind = \"session-name\", format = \"[{name}]\", style = { fg = \"red\", bold = true } }, { kind = \"time\", format = \" %H:%M\" }]\n".to_owned(),
        ] {
            let report = run(&input);
            assert!(report.is_ok(), "false positives for {input:?}: {:?}", report.findings);
        }
    }

    /// Each fault class: located at its full path, classified, and carrying
    /// the fix-oriented text (suggestion, maximum, valid keys).
    #[test]
    fn each_mistake_is_located_classified_and_explained() {
        let floor = phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES.to_string();
        let cases: &[(&str, &str, Fault, &[&str])] = &[
            (
                "[sidebar]\nenabledd = true\n",
                "sidebar.enabledd",
                Fault::UnknownKey,
                &["expected one of"],
            ),
            // A key removed in an earlier release is an ordinary unknown key.
            (
                "[defaults]\nrefresh-rate = 60\n",
                "defaults.refresh-rate",
                Fault::UnknownKey,
                &[],
            ),
            (
                "[keybindings]\nwhich-key = \"yes\"\n",
                "keybindings.which-key",
                Fault::BadValue,
                &[],
            ),
            (
                "[defaults]\nhistory-bytes = 134217728\n",
                "defaults.history-bytes",
                Fault::BadValue,
                &["67108864"],
            ),
            (
                "[limits]\nmetadata-value-bytes = 0\n",
                "limits.metadata-value-bytes",
                Fault::BadValue,
                &[&floor],
            ),
            (
                "[keybindings.prefix-table]\nq = \"kill-pain\"\n",
                "keybindings.prefix-table.q",
                Fault::UnknownName,
                &["unknown action `kill-pain` (did you mean `kill-pane`?)"],
            ),
            (
                "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n",
                "keybindings.prefix-table.q-",
                Fault::BadChord,
                &[],
            ),
            (
                "[keybindings]\nprefix = \"Ctrl-a\"\n",
                "keybindings.prefix",
                Fault::BadChord,
                &[],
            ),
            (
                "[keybindings.prefix-table]\nR = { action = \"resize-pain\", direction = \"left\" }\n",
                "keybindings.prefix-table.R",
                Fault::UnknownName,
                &["`resize-pane`"],
            ),
            (
                "[keybindings.prefix-table]\n\"y\" = \"copy-mode\"\n\"y x\" = \"kill-pane\"\n",
                "keybindings.prefix-table.\"y x\"",
                Fault::BadChord,
                &["ambiguous prefix"],
            ),
            (
                "[[hooks.pane-exited]]\naction = \"noop\"\n",
                "hooks.pane-exited",
                Fault::UnknownName,
                &["unknown hook event `pane-exited` (did you mean `pane-exit`?)"],
            ),
            (
                "[[hooks.pane-exit]]\nwhen = { exitcode = 0 }\naction = { kind = \"run\", command = \"true\" }\n",
                "hooks.pane-exit[0].when.exitcode",
                Fault::UnknownName,
                &[
                    "context keys are exit-code, terminal-id",
                    "did you mean `exit-code`?",
                ],
            ),
            // `-startswith` strips before the lookup; the full spelling is reported.
            (
                "[[hooks.after-new-pane]]\nwhen = { cwd-startswith = \"/x\" }\naction = \"noop\"\n",
                "hooks.after-new-pane[0].when.cwd-startswith",
                Fault::UnknownName,
                &["unknown when key `cwd-startswith`"],
            ),
            (
                "[[hooks.pane-exit]]\naction = { kind = \"run\", command = \"true\" }\n\n\
              [[hooks.pane-exit]]\naction = { kind = \"message\", text = \"bye\" }\n",
                "hooks.pane-exit[1].action",
                Fault::DeadAction,
                &["action `message` never executes server-side"],
            ),
            (
                "[[hooks.pane-exit]]\naction = { kind = \"run\", command = [] }\n",
                "hooks.pane-exit[0].action",
                Fault::DeadAction,
                &["no usable `command`"],
            ),
            (
                "[status]\nleft = [\"windws\"]\n",
                "status.left[0]",
                Fault::UnknownName,
                &["unknown widget kind `windws` (did you mean `windows`?)"],
            ),
            (
                "[status]\nright = [{ kind = \"time\", formt = \"%H\" }]\n",
                "status.right[0]",
                Fault::BadValue,
                &[
                    "widget time",
                    "unknown option `formt`",
                    "did you mean `format`?",
                ],
            ),
            (
                "[status]\ncenter = [{ kind = \"session-name\", style = { colour = \"red\" } }]\n",
                "status.center[0]",
                Fault::BadValue,
                &["`style` must be a style table"],
            ),
        ];
        for &(input, path, fault, needles) in cases {
            let report = run(input);
            assert_eq!(paths(&report), vec![path], "{input:?}");
            let finding = &report.findings[0];
            assert_eq!(finding.fault, fault, "{input:?}");
            for needle in needles {
                assert!(
                    finding.message.contains(needle),
                    "{input:?}: {}",
                    finding.message
                );
            }
            // Schema and semantic findings alike name the file to open.
            assert_eq!(
                finding.source,
                Some(LayerSource::User(PATH.into())),
                "{input:?}: origin was {}",
                finding.origin()
            );
        }
    }

    /// Every mistake in one pass, not one per edit-run cycle.
    #[test]
    fn every_finding_is_reported_in_one_pass() {
        let report = run(
            "[sidebar]\nenabledd = true\nwidht = 4\n\n[keybindings]\nwich-key = true\nwhich-key = \"yes\"\n",
        );
        let found = paths(&report);
        for want in [
            "sidebar.enabledd",
            "sidebar.widht",
            "keybindings.wich-key",
            "keybindings.which-key",
        ] {
            assert!(found.contains(&want), "missing {want} in {found:?}");
        }
        assert!(!report.truncated);

        let report = run(
            "[status]\nleft = [\"windws\", { kind = \"time\", formt = \"%H\" }]\nright = [\"not-even-close\"]\n",
        );
        assert_eq!(
            paths(&report),
            vec!["status.left[0]", "status.left[1]", "status.right[0]"]
        );
    }

    /// "No findings" must never be said about a file that was never read.
    #[test]
    fn unparseable_toml_is_an_error_not_a_clean_report() {
        assert!(check("this is not = = toml\n", Path::new(PATH)).is_err());
    }
}
