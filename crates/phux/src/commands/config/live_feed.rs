//! Live agent-state feed for the `phux config agents` projection
//! (phux-r82.10).
//!
//! Manifest `[[agents]]` entries are declarative templates (ADR-0040): a
//! plugin says "this agent exists and here is its declared baseline". The
//! runtime truth lives elsewhere — in the per-pane `phux.agent/v1` L3
//! record (ADR-0040) and the ADR-0035 asked machinery. This module reads
//! both from a running server, best-effort, and merges them into the rows
//! the projection prints: runtime values override the manifest baseline,
//! and the manifest stays as the declared fallback when no runtime record
//! matches (or no server is running at all).
//!
//! Precedence inside a matched pane follows ADR-0040: the record outranks
//! the `phux-ask` title sentinel, so an active ask elevates a binding to
//! `blocked` only when the record declares no state of its own. This is
//! projection-side composition of existing reads (ADR-0030): one
//! `GET_STATE` plus the pipelined per-pane `GET_METADATA` index — no wire
//! change.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
use phux_config::plugin::{PluginAgentAttention, PluginAgentState, PluginManifestAgent};
use phux_protocol::ids::TerminalId;
use serde::Serialize;

use crate::commands::agent::{fetch_agent_index, format_terminal};

/// Everything the projection can learn from a running server.
#[derive(Debug, Default)]
pub(super) struct LiveAgentFeed {
    /// Decoded `phux.agent/v1` records by pane.
    pub(super) records: HashMap<TerminalId, AgentRecord>,
    /// Panes whose title carries an active ADR-0035 `phux-ask` sentinel.
    pub(super) asked: HashSet<TerminalId>,
}

/// Best-effort fetch of the live feed from the server at `socket_path`.
///
/// Returns `None` when no server answers — `phux config agents` must keep
/// working offline, reporting declared manifest values — and never
/// surfaces transport errors: a partially readable server degrades to
/// whatever was collected.
pub(super) async fn fetch_live_feed(socket_path: &Path) -> Option<LiveAgentFeed> {
    // `into_snapshot_ignoring_degradation`: this feed *overlays* live evidence
    // onto the config-declared agent list and never claims a pane is absent —
    // a satellite the hub could not reach simply contributes no records, which
    // is the same outcome this function already documents for "no server" and
    // for a pane with no `phux.agent/v1` value. Nothing downstream turns a
    // missing record into a negative assertion, so there is no false statement
    // to prevent here. (The audit-trail `warn!` inside still fires.)
    let Ok(view) = phux_client::state::get_state(socket_path).await else {
        return None;
    };
    let snapshot = view.into_snapshot_ignoring_degradation();
    let records = fetch_agent_index(socket_path, &snapshot).await;
    let asked = snapshot
        .panes
        .iter()
        .filter(|pane| pane.title.as_deref().is_some_and(is_ask_sentinel))
        .map(|pane| pane.id.clone())
        .collect();
    Some(LiveAgentFeed { records, asked })
}

/// The ADR-0035 title-sentinel predicate — the same shape the `phux
/// agent` detector matches (`phux-ask…:…`).
fn is_ask_sentinel(title: &str) -> bool {
    title.starts_with("phux-ask") && title.contains(':')
}

/// One manifest-declared agent entry, flattened for the merge.
#[derive(Debug, Clone)]
pub(super) struct ManifestAgentRow {
    pub(super) plugin_id: String,
    pub(super) plugin_enabled: bool,
    pub(super) agent: PluginManifestAgent,
}

/// Which side of the merge produced the effective `state`/`attention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ProjectionSource {
    /// A live `phux.agent/v1` record matched this agent.
    Runtime,
    /// No runtime record matched; declared manifest values are reported.
    Manifest,
}

/// The declared manifest baseline, kept alongside the effective values so
/// a consumer can always see what the plugin promised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(super) struct DeclaredValues {
    pub(super) state: PluginAgentState,
    pub(super) attention: PluginAgentAttention,
}

/// The matched live record, projected for output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct RuntimeBinding {
    /// Pane the record was read from (`@N` or `host/@N`).
    pub(super) terminal: String,
    /// The record's human-facing agent name.
    pub(super) name: String,
    /// The record's kind slug, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<String>,
    /// Record state, after asked elevation (see module docs).
    pub(super) state: AgentMetaState,
    /// Effective attention (declared on the record, else derived from
    /// `state` per the `phux.agent/v1` convention).
    pub(super) attention: AgentAttention,
    /// The record's free-form session label, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) session: Option<String>,
    /// True when the pane has an active ADR-0035 ask pending.
    pub(super) asked: bool,
}

/// One row of the merged `phux config agents` projection
/// (`schema_version = 2` of the agents JSON document).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct AgentProjection {
    pub(super) plugin_id: String,
    pub(super) plugin_enabled: bool,
    pub(super) id: String,
    pub(super) label: String,
    pub(super) description: Option<String>,
    /// Effective lifecycle state: runtime when a record matched, else the
    /// declared manifest value.
    pub(super) state: &'static str,
    /// Effective attention: runtime when a record matched, else the
    /// declared manifest value.
    pub(super) attention: &'static str,
    /// Which side won the merge.
    pub(super) source: ProjectionSource,
    /// Declared manifest fallback values (always present).
    pub(super) declared: DeclaredValues,
    /// The matched live record, when one exists.
    pub(super) runtime: Option<RuntimeBinding>,
    pub(super) contexts: Vec<String>,
}

/// Merge manifest-declared agents with the live feed. Pure: the async
/// fetch stays in [`fetch_live_feed`], so merge semantics are unit-testable
/// without a server.
pub(super) fn merge_agents(
    rows: &[ManifestAgentRow],
    feed: Option<&LiveAgentFeed>,
) -> Vec<AgentProjection> {
    rows.iter().map(|row| project_row(row, feed)).collect()
}

fn project_row(row: &ManifestAgentRow, feed: Option<&LiveAgentFeed>) -> AgentProjection {
    let runtime = feed.and_then(|feed| best_binding(&row.agent.id, feed));
    let (state, attention, source) = runtime.as_ref().map_or_else(
        || {
            (
                declared_state_word(row.agent.state),
                declared_attention_word(row.agent.attention),
                ProjectionSource::Manifest,
            )
        },
        |binding| {
            (
                binding.state.as_str(),
                binding.attention.as_str(),
                ProjectionSource::Runtime,
            )
        },
    );
    AgentProjection {
        plugin_id: row.plugin_id.clone(),
        plugin_enabled: row.plugin_enabled,
        id: row.agent.id.clone(),
        label: row.agent.label.clone(),
        description: row.agent.description.clone(),
        state,
        attention,
        source,
        declared: DeclaredValues {
            state: row.agent.state,
            attention: row.agent.attention,
        },
        runtime,
        contexts: row.agent.contexts.clone(),
    }
}

/// All live records whose identity slug matches `agent_id`, reduced to
/// the most attention-worthy binding (rank by attention, then state
/// severity, then pane id for determinism).
fn best_binding(agent_id: &str, feed: &LiveAgentFeed) -> Option<RuntimeBinding> {
    let mut bindings: Vec<RuntimeBinding> = feed
        .records
        .iter()
        .filter(|(_, record)| record_slug(record) == agent_id)
        .map(|(pane, record)| binding_for(pane, record, feed.asked.contains(pane)))
        .collect();
    bindings.sort_by(|a, b| {
        attention_rank(b.attention)
            .cmp(&attention_rank(a.attention))
            .then_with(|| state_rank(b.state).cmp(&state_rank(a.state)))
            .then_with(|| a.terminal.cmp(&b.terminal))
    });
    bindings.into_iter().next()
}

/// The identity slug a record advertises: its `kind`, else the lowercased
/// name — the same derivation the `phux agent` detector uses (ADR-0040).
fn record_slug(record: &AgentRecord) -> String {
    record
        .kind
        .clone()
        .unwrap_or_else(|| record.name.to_lowercase())
}

fn binding_for(pane: &TerminalId, record: &AgentRecord, asked: bool) -> RuntimeBinding {
    // ADR-0040: the record outranks the ask-title sentinel, so an active
    // ask elevates to blocked only when the record declares no state.
    let state = if asked && record.state == AgentMetaState::Unknown {
        AgentMetaState::Blocked
    } else {
        record.state
    };
    // Route the (possibly elevated) state through the record's own
    // derivation so a declared attention still wins and an elevated
    // blocked derives high — one source of truth for the convention.
    let attention = AgentRecord {
        state,
        ..record.clone()
    }
    .effective_attention();
    RuntimeBinding {
        terminal: format_terminal(pane),
        name: record.name.clone(),
        kind: record.kind.clone(),
        state,
        attention,
        session: record.session.clone(),
        asked,
    }
}

const fn attention_rank(attention: AgentAttention) -> u8 {
    match attention {
        AgentAttention::None => 0,
        AgentAttention::Low => 1,
        AgentAttention::Normal => 2,
        AgentAttention::High => 3,
    }
}

const fn state_rank(state: AgentMetaState) -> u8 {
    match state {
        AgentMetaState::Unknown => 0,
        AgentMetaState::Idle => 1,
        AgentMetaState::Done => 2,
        AgentMetaState::Working => 3,
        AgentMetaState::Blocked => 4,
    }
}

/// The kebab-case display/JSON word for a declared manifest state.
pub(super) const fn declared_state_word(state: PluginAgentState) -> &'static str {
    match state {
        PluginAgentState::Unknown => "unknown",
        PluginAgentState::Idle => "idle",
        PluginAgentState::Working => "working",
        PluginAgentState::Blocked => "blocked",
    }
}

/// The kebab-case display/JSON word for a declared manifest attention.
pub(super) const fn declared_attention_word(attention: PluginAgentAttention) -> &'static str {
    match attention {
        PluginAgentAttention::None => "none",
        PluginAgentAttention::Low => "low",
        PluginAgentAttention::Normal => "normal",
        PluginAgentAttention::High => "high",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn manifest_row(agent_id: &str, state: PluginAgentState) -> ManifestAgentRow {
        ManifestAgentRow {
            plugin_id: "example.agent-tools".to_owned(),
            plugin_enabled: true,
            agent: PluginManifestAgent {
                id: agent_id.to_owned(),
                label: "Codex".to_owned(),
                description: Some("Coding agent".to_owned()),
                state,
                attention: PluginAgentAttention::Normal,
                contexts: vec!["workspace".to_owned()],
            },
        }
    }

    fn pane(id: u32) -> TerminalId {
        TerminalId::Local { id }
    }

    fn record(name: &str, kind: Option<&str>, state: AgentMetaState) -> AgentRecord {
        AgentRecord {
            name: name.to_owned(),
            kind: kind.map(str::to_owned),
            state,
            attention: None,
            session: None,
        }
    }

    fn feed_with(entries: Vec<(TerminalId, AgentRecord)>, asked: Vec<TerminalId>) -> LiveAgentFeed {
        LiveAgentFeed {
            records: entries.into_iter().collect(),
            asked: asked.into_iter().collect(),
        }
    }

    /// Merge semantics: a matching runtime record overrides the declared
    /// manifest state, and the declared values remain visible as fallback.
    #[test]
    fn runtime_record_overrides_manifest_state() {
        let rows = vec![manifest_row("codex", PluginAgentState::Idle)];
        let feed = feed_with(
            vec![(
                pane(3),
                record("Reviewer", Some("codex"), AgentMetaState::Working),
            )],
            vec![],
        );

        let merged = merge_agents(&rows, Some(&feed));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].state, "working");
        assert_eq!(merged[0].attention, "normal");
        assert_eq!(merged[0].source, ProjectionSource::Runtime);
        assert_eq!(merged[0].declared.state, PluginAgentState::Idle);
        let binding = merged[0].runtime.as_ref().expect("runtime binding");
        assert_eq!(binding.terminal, "@3");
        assert_eq!(binding.name, "Reviewer");
        assert!(!binding.asked);
    }

    /// Attention propagation: a blocked record with no declared attention
    /// derives high attention into the projection, overriding the
    /// manifest's declared normal.
    #[test]
    fn attention_propagates_from_runtime_record() {
        let rows = vec![manifest_row("codex", PluginAgentState::Working)];
        let feed = feed_with(
            vec![(
                pane(1),
                record("codex", Some("codex"), AgentMetaState::Blocked),
            )],
            vec![],
        );

        let merged = merge_agents(&rows, Some(&feed));

        assert_eq!(merged[0].state, "blocked");
        assert_eq!(merged[0].attention, "high");
        assert_eq!(merged[0].source, ProjectionSource::Runtime);
    }

    /// Asked propagation: an active ADR-0035 ask elevates a record that
    /// declares no state to blocked/high and flags the binding.
    #[test]
    fn asked_pane_elevates_stateless_record_to_blocked() {
        let rows = vec![manifest_row("codex", PluginAgentState::Idle)];
        let feed = feed_with(
            vec![(
                pane(2),
                record("codex", Some("codex"), AgentMetaState::Unknown),
            )],
            vec![pane(2)],
        );

        let merged = merge_agents(&rows, Some(&feed));

        assert_eq!(merged[0].state, "blocked");
        assert_eq!(merged[0].attention, "high");
        let binding = merged[0].runtime.as_ref().expect("runtime binding");
        assert!(binding.asked);
    }

    /// ADR-0040: the record outranks the ask sentinel — a declared state
    /// survives an active ask, though the ask itself is still reported.
    #[test]
    fn declared_record_state_outranks_ask_sentinel() {
        let rows = vec![manifest_row("codex", PluginAgentState::Idle)];
        let feed = feed_with(
            vec![(
                pane(2),
                record("codex", Some("codex"), AgentMetaState::Working),
            )],
            vec![pane(2)],
        );

        let merged = merge_agents(&rows, Some(&feed));

        assert_eq!(merged[0].state, "working");
        let binding = merged[0].runtime.as_ref().expect("runtime binding");
        assert!(binding.asked, "the pending ask must stay visible");
    }

    /// Fallback: no matching runtime record (wrong slug, or no feed at
    /// all) reports the declared manifest values with manifest provenance.
    #[test]
    fn falls_back_to_manifest_without_runtime_record() {
        let rows = vec![manifest_row("codex", PluginAgentState::Working)];
        let feed = feed_with(
            vec![(
                pane(1),
                record("other", Some("claude"), AgentMetaState::Blocked),
            )],
            vec![],
        );

        for feed in [Some(&feed), None] {
            let merged = merge_agents(&rows, feed);
            assert_eq!(merged[0].state, "working");
            assert_eq!(merged[0].attention, "normal");
            assert_eq!(merged[0].source, ProjectionSource::Manifest);
            assert_eq!(merged[0].runtime, None);
        }
    }

    /// A record with no kind slug matches via its lowercased name — the
    /// same identity derivation the `phux agent` detector uses.
    #[test]
    fn kindless_record_matches_by_lowercased_name() {
        let rows = vec![manifest_row("codex", PluginAgentState::Idle)];
        let feed = feed_with(
            vec![(pane(4), record("Codex", None, AgentMetaState::Working))],
            vec![],
        );

        let merged = merge_agents(&rows, Some(&feed));

        assert_eq!(merged[0].source, ProjectionSource::Runtime);
        assert_eq!(merged[0].state, "working");
    }

    /// Several panes declaring the same agent reduce to the most
    /// attention-worthy binding.
    #[test]
    fn most_attention_worthy_pane_wins() {
        let rows = vec![manifest_row("codex", PluginAgentState::Idle)];
        let feed = feed_with(
            vec![
                (
                    pane(1),
                    record("codex", Some("codex"), AgentMetaState::Idle),
                ),
                (
                    pane(2),
                    record("codex", Some("codex"), AgentMetaState::Blocked),
                ),
                (
                    pane(3),
                    record("codex", Some("codex"), AgentMetaState::Working),
                ),
            ],
            vec![],
        );

        let merged = merge_agents(&rows, Some(&feed));

        let binding = merged[0].runtime.as_ref().expect("runtime binding");
        assert_eq!(binding.terminal, "@2");
        assert_eq!(merged[0].state, "blocked");
        assert_eq!(merged[0].attention, "high");
    }
}
