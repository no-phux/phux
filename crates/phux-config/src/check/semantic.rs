//! The semantic pass: mistakes that deserialize fine and do nothing at
//! runtime.

use std::collections::BTreeMap;

use super::{Fault, Finding, array_entry_source, push_semantic, suggest};
use crate::keybind::{KeybindError, Resolver, parse_chord, parse_chord_sequence};
use crate::widget::{WidgetError, WidgetRegistry};
use crate::{Config, ConfigProvenance, DefaultsCfg, HookEntry, KeybindingsCfg, theme_slots, vocab};

pub(super) fn semantic_pass(
    config: &Config,
    provenance: &ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    defaults_findings(&config.defaults, provenance, findings);
    limits_findings(&config.limits, provenance, findings);
    keybinding_findings(&config.keybindings, provenance, findings);
    hook_findings(&config.hooks, provenance, findings);
    status_widget_findings(&config.status, provenance, findings);
    theme_findings(&config.theme, provenance, findings);
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

/// Chords must parse and action names must exist, in every binding table
/// (key modes included); `enter-mode` must name a defined mode, and mode
/// options must belong to one. Parameterized-action arguments are
/// otherwise validated by the dispatcher, not here.
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

    for (table, bindings) in binding_tables(kb) {
        for (binding, action) in bindings {
            let path = binding_path(&table, binding);
            if let Err(error) = parse_chord_sequence(binding) {
                push_semantic(
                    findings,
                    provenance,
                    path.clone(),
                    Fault::BadChord,
                    error.to_string(),
                );
            }
            let name = vocab::action_name(action);
            if !vocab::ACTION_NAMES.contains(&name) {
                let message = format!(
                    "unknown action `{name}`{}",
                    suggest(name, vocab::ACTION_NAMES)
                );
                push_semantic(findings, provenance, path, Fault::UnknownName, message);
            }
        }
    }

    for name in kb
        .mode_options
        .keys()
        .filter(|n| !kb.modes.contains_key(*n))
    {
        push_semantic(
            findings,
            provenance,
            crate::layer::child_path("keybindings.mode-options", name),
            Fault::UnknownName,
            format!(
                "options for key mode `{name}`, which no [keybindings.modes.{name}] table defines"
            ),
        );
    }

    // From the resolver: ambiguity and key-mode problems. Per-binding syntax
    // and action-name errors were reported above.
    let (_, diagnostics) = Resolver::new_lenient(kb);
    for diagnostic in diagnostics {
        let fault = match diagnostic.error {
            KeybindError::AmbiguousPrefix(_) => Fault::BadChord,
            KeybindError::BadModeName(_) | KeybindError::MissingModeName => Fault::BadValue,
            KeybindError::UnknownMode { .. } => Fault::UnknownName,
            _ => continue,
        };
        push_semantic(
            findings,
            provenance,
            diagnostic_path(kb, &diagnostic.binding),
            fault,
            diagnostic.error.to_string(),
        );
    }
}

/// Every binding table under `[keybindings]` by its path below it:
/// `global`, `prefix-table`, then `modes.<name>` per key mode.
fn binding_tables(kb: &KeybindingsCfg) -> Vec<(String, &BTreeMap<String, crate::Action>)> {
    let mut tables = vec![
        ("global".to_owned(), &kb.global),
        ("prefix-table".to_owned(), &kb.prefix_table),
    ];
    tables.extend(
        kb.modes
            .iter()
            .map(|(name, table)| (crate::layer::child_path("modes", name), table)),
    );
    tables
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
            for key in vocab::unknown_hook_when_keys(context_keys, &entry.when) {
                findings.push(Finding {
                    path: crate::layer::child_path(&format!("{entry_path}.when"), key),
                    fault: Fault::UnknownName,
                    message: unknown_when_key_message(event, key, context_keys),
                    source: source.clone(),
                });
            }
            if let Some(name) = vocab::dead_hook_action(&entry.action) {
                findings.push(Finding {
                    path: format!("{entry_path}.action"),
                    fault: Fault::DeadAction,
                    message: dead_hook_action_message(name),
                    source,
                });
            }
        }
    }
}

fn unknown_when_key_message(event: &str, key: &str, context_keys: &[&str]) -> String {
    format!(
        "unknown when key `{key}`: this clause can never match; \
         `{event}` context keys are {}{}",
        context_keys.join(", "),
        suggest(vocab::hook_when_key_base(key), context_keys),
    )
}

/// Why a dead hook action (see [`vocab::dead_hook_action`]) never runs.
fn dead_hook_action_message(name: &str) -> String {
    if name == "run" {
        return "`run` action has no usable `command` (need a non-blank string or a \
                non-empty array of strings); a match consumes the event and runs nothing"
            .to_owned();
    }
    format!(
        "action `{name}` never executes server-side (only `run` does; \
         `noop` is the deliberate no-op); a match still consumes the event"
    )
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
            let Err(error) = registry.build(&entry.to_spec()) else {
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

/// Dotted path for a binding key, quoted the way provenance records it.
fn binding_path(table: &str, binding: &str) -> String {
    crate::layer::child_path(&format!("keybindings.{table}"), binding)
}

/// Locate a resolver diagnostic's binding. A key mode's reads
/// `mode:<name> <key>` (`mode:<name>` for the mode itself). The
/// prefix-conflict case reports the global binding that can never fire, so
/// `global` is checked first; a match in neither table is the prefix itself.
fn diagnostic_path(kb: &KeybindingsCfg, binding: &str) -> String {
    if let Some(rest) = binding.strip_prefix("mode:") {
        let modes = "keybindings.modes";
        return match rest.split_once(' ') {
            Some((mode, key)) => {
                crate::layer::child_path(&crate::layer::child_path(modes, mode), key)
            }
            None => crate::layer::child_path(modes, rest),
        };
    }
    if kb.global.contains_key(binding) {
        binding_path("global", binding)
    } else if kb.prefix_table.contains_key(binding) {
        binding_path("prefix-table", binding)
    } else {
        "keybindings.prefix".to_owned()
    }
}

/// `[theme]` is a free-form `slot -> color` map, so a misspelt slot or a
/// colour the renderer cannot parse deserializes fine and is dropped at
/// paint time (with only a log warning). `name` and `file` select a theme
/// rather than override a slot, so they are not slots.
fn theme_findings(
    theme: &crate::ThemeCfg,
    provenance: &ConfigProvenance,
    findings: &mut Vec<Finding>,
) {
    let slots = theme_slots::slot_names();
    for (key, spec) in theme.slot_overrides() {
        let path = crate::layer::child_path("theme", key);
        if !theme_slots::is_slot(key) {
            let message = format!(
                "unknown theme slot `{key}`{}; the override is ignored",
                suggest(key, &slots)
            );
            push_semantic(findings, provenance, path, Fault::UnknownName, message);
        } else if !theme_slots::is_color(spec) {
            let message = format!(
                "`{spec}` is not a colour (use a name like `cyan`, an index 0-255, or \
                 `#rrggbb`); the slot keeps its default"
            );
            push_semantic(findings, provenance, path, Fault::BadValue, message);
        }
    }
}
