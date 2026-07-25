use phux_client::agent_meta::AgentRecord;

use super::model::{
    AgentIdentity, AgentKind, AgentSource, AgentState, AgentStateReport, PaneEvidence, PluginAgent,
    StateSignal, attention_for, identity, plugin_attention, record_attention, record_state,
};

pub(super) fn infer_agent_state(
    evidence: &PaneEvidence,
    plugins: &[PluginAgent],
) -> AgentStateReport {
    // ADR-0040: a declared `phux.agent/v1` record is the authoritative
    // source — no title or screen substring matching is consulted at all.
    // Heuristics below remain the compatibility path for panes without one.
    if let Some(record) = &evidence.record {
        return report_from_record(evidence, plugins, record);
    }
    let mut sources = Vec::new();
    let agent = infer_identity(evidence, plugins, &mut sources);
    let plugin = plugins.iter().find(|plugin| plugin.id == agent.id);
    let mut state = infer_state(evidence, &mut sources);
    if let Some(plugin) = plugin {
        sources.push(AgentSource {
            kind: "plugin_report",
            signal: "configured agent declaration".to_owned(),
            confidence: 0.55,
            observed: format!("{} reports {:?}", plugin.id, plugin.state),
        });
        if state.state == AgentState::Unknown {
            state = StateSignal::from_plugin(plugin.state);
        }
    }
    sources.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    AgentStateReport {
        terminal: evidence.terminal.clone(),
        session: evidence.session.clone(),
        window: evidence.window.clone(),
        agent,
        state: state.state,
        confidence: state.confidence,
        attention: plugin.map_or_else(
            || attention_for(state.state),
            |p| plugin_attention(p.attention),
        ),
        title: evidence.title.clone(),
        cwd: evidence.cwd.clone(),
        sources,
        explanation: state.explanation,
    }
}

/// ADR-0040: build the report straight from the structured record. The
/// single source entry carries provenance (`agent_record`) so `agent
/// explain` shows exactly why no heuristic ran.
fn report_from_record(
    evidence: &PaneEvidence,
    plugins: &[PluginAgent],
    record: &AgentRecord,
) -> AgentStateReport {
    let slug = record
        .kind
        .clone()
        .unwrap_or_else(|| record.name.to_lowercase());
    let kind = match slug.as_str() {
        "codex" => AgentKind::Codex,
        "claude" => AgentKind::Claude,
        "opencode" => AgentKind::OpenCode,
        "pi" => AgentKind::Pi,
        "omp" => AgentKind::Omp,
        other if plugins.iter().any(|plugin| plugin.id == other) => AgentKind::Plugin,
        _ => AgentKind::Declared,
    };
    let state = record_state(record.state);
    AgentStateReport {
        terminal: evidence.terminal.clone(),
        session: evidence.session.clone(),
        window: evidence.window.clone(),
        agent: identity(&slug, &record.name, kind),
        state,
        confidence: 0.98,
        attention: record_attention(record.effective_attention()),
        title: evidence.title.clone(),
        cwd: evidence.cwd.clone(),
        sources: vec![AgentSource {
            kind: "agent_record",
            signal: "phux.agent/v1 metadata record".to_owned(),
            confidence: 0.98,
            observed: String::from_utf8(record.encode()).unwrap_or_default(),
        }],
        explanation: "declared via the phux.agent/v1 L3 record (ADR-0040)".to_owned(),
    }
}

fn infer_identity(
    evidence: &PaneEvidence,
    plugins: &[PluginAgent],
    sources: &mut Vec<AgentSource>,
) -> AgentIdentity {
    let text = evidence_text(evidence);
    if contains_token(&text, "codex") {
        sources.push(AgentSource {
            kind: "identity",
            signal: "codex marker".to_owned(),
            confidence: 0.8,
            observed: "Codex".to_owned(),
        });
        return identity("codex", "Codex", AgentKind::Codex);
    }
    if contains_token(&text, "claude") {
        sources.push(AgentSource {
            kind: "identity",
            signal: "claude marker".to_owned(),
            confidence: 0.8,
            observed: "Claude".to_owned(),
        });
        return identity("claude", "Claude", AgentKind::Claude);
    }
    for plugin in plugins {
        if contains_token(&text, &plugin.id) || contains_token(&text, &plugin.label) {
            sources.push(AgentSource {
                kind: "identity",
                signal: "plugin marker".to_owned(),
                confidence: 0.65,
                observed: plugin.label.clone(),
            });
            return identity(&plugin.id, &plugin.label, AgentKind::Plugin);
        }
    }
    identity("unknown", "Unknown agent", AgentKind::Unknown)
}

fn infer_state(evidence: &PaneEvidence, sources: &mut Vec<AgentSource>) -> StateSignal {
    if let Some(title) = evidence.title.as_deref()
        && title.starts_with("phux-ask")
        && title.contains(':')
    {
        sources.push(AgentSource {
            kind: "title_ask",
            signal: "phux-ask title sentinel".to_owned(),
            confidence: 0.95,
            observed: title.to_owned(),
        });
        return StateSignal::new(
            AgentState::Blocked,
            0.95,
            "waiting on a reported human-answerable ask",
        );
    }
    let text = evidence_text(evidence);
    if has_any(
        &text,
        &[
            "blocked",
            "need approval",
            "permission",
            "approve",
            "continue?",
        ],
    ) {
        sources.push(AgentSource {
            kind: "screen",
            signal: "blocked prompt words".to_owned(),
            confidence: 0.78,
            observed: "approval prompt".to_owned(),
        });
        return StateSignal::new(AgentState::Blocked, 0.78, "screen asks for human input");
    }
    if has_any(
        &text,
        &["done", "complete", "completed", "success", "tests passed"],
    ) {
        sources.push(AgentSource {
            kind: "screen",
            signal: "completion words".to_owned(),
            confidence: 0.72,
            observed: "completion text".to_owned(),
        });
        return StateSignal::new(AgentState::Done, 0.72, "screen reports completion");
    }
    if evidence.semantic_input {
        sources.push(AgentSource {
            kind: "semantic_cells",
            signal: "OSC-133 input cells".to_owned(),
            confidence: 0.68,
            observed: "input region".to_owned(),
        });
        return StateSignal::new(AgentState::Working, 0.68, "recent command input is visible");
    }
    if has_any(
        &text,
        &["running", "working", "thinking", "building", "compiling"],
    ) {
        sources.push(AgentSource {
            kind: "screen",
            signal: "active work words".to_owned(),
            confidence: 0.64,
            observed: "work text".to_owned(),
        });
        return StateSignal::new(AgentState::Working, 0.64, "screen suggests active work");
    }
    if evidence.lines.iter().any(|line| !line.trim().is_empty()) {
        sources.push(AgentSource {
            kind: "screen",
            signal: "visible quiet screen".to_owned(),
            confidence: 0.45,
            observed: "no active cue".to_owned(),
        });
        return StateSignal::new(AgentState::Idle, 0.45, "no blocking or working cue found");
    }
    StateSignal::new(AgentState::Unknown, 0.25, "no public agent cue found")
}

fn evidence_text(evidence: &PaneEvidence) -> String {
    let mut parts = Vec::with_capacity(evidence.lines.len().saturating_add(1));
    if let Some(title) = &evidence.title {
        parts.push(title.as_str());
    }
    parts.extend(evidence.lines.iter().map(String::as_str));
    parts.join("\n").to_lowercase()
}

fn contains_token(haystack: &str, needle: &str) -> bool {
    haystack.contains(&needle.to_lowercase())
}

fn has_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::infer_agent_state;
    use crate::commands::agent::model::{AgentKind, AgentState, PaneEvidence};

    #[test]
    fn infers_codex_blocked_from_ask_title() {
        let evidence = PaneEvidence::for_test(
            "@7",
            Some("phux-ask[deploy]:Approve deploy??s=Yes|No"),
            &["Codex is waiting"],
        );

        let state = infer_agent_state(&evidence, &[]);

        assert_eq!(state.agent.kind, AgentKind::Codex);
        assert_eq!(state.state, AgentState::Blocked);
        assert!(state.confidence >= 0.9);
        assert_eq!(state.sources[0].kind, "title_ask");
    }

    #[test]
    fn infers_claude_done_from_screen() {
        let evidence = PaneEvidence::for_test(
            "@8",
            Some("Claude Code"),
            &["All tasks complete", "tests passed"],
        );

        let state = infer_agent_state(&evidence, &[]);

        assert_eq!(state.agent.kind, AgentKind::Claude);
        assert_eq!(state.state, AgentState::Done);
        assert!(state.sources.iter().any(|source| source.kind == "screen"));
    }

    /// ADR-0040: a declared record outranks every heuristic — the title is
    /// a `phux-ask` sentinel AND the screen screams "codex blocked", but the
    /// structured record says a working Claude and that is what reports.
    #[test]
    fn declared_record_outranks_title_and_screen_heuristics() {
        use phux_client::agent_meta::{AgentMetaState, AgentRecord};
        let mut evidence = PaneEvidence::for_test(
            "@5",
            Some("phux-ask[x]:Approve??s=Yes|No"),
            &["codex blocked need approval"],
        );
        evidence.record = Some(AgentRecord {
            name: "Reviewer".to_owned(),
            kind: Some("claude".to_owned()),
            state: AgentMetaState::Working,
            ..AgentRecord::default()
        });

        let state = infer_agent_state(&evidence, &[]);

        assert_eq!(state.agent.kind, AgentKind::Claude);
        assert_eq!(state.agent.label, "Reviewer");
        assert_eq!(state.state, AgentState::Working);
        assert_eq!(state.sources.len(), 1, "no heuristic source may run");
        assert_eq!(state.sources[0].kind, "agent_record");
    }

    #[test]
    fn shipped_detector_kinds_have_first_class_cli_identities() {
        use phux_client::agent_meta::{AgentMetaState, AgentRecord};

        for (slug, expected) in [
            ("codex", AgentKind::Codex),
            ("claude", AgentKind::Claude),
            ("opencode", AgentKind::OpenCode),
            ("pi", AgentKind::Pi),
            ("omp", AgentKind::Omp),
        ] {
            let mut evidence = PaneEvidence::for_test("@6", None, &[]);
            evidence.record = Some(AgentRecord {
                name: slug.to_owned(),
                kind: Some(slug.to_owned()),
                state: AgentMetaState::Idle,
                ..AgentRecord::default()
            });

            let state = infer_agent_state(&evidence, &[]);
            assert_eq!(state.agent.kind, expected, "{slug}");
        }
    }

    /// ADR-0040: an unrecognized kind slug still reports as a first-class
    /// declared identity, not `unknown`.
    #[test]
    fn declared_record_with_custom_kind_is_declared_not_unknown() {
        use phux_client::agent_meta::{AgentMetaState, AgentRecord};
        let mut evidence = PaneEvidence::for_test("@6", None, &[]);
        evidence.record = Some(AgentRecord {
            name: "herdr-worker".to_owned(),
            kind: Some("herdr".to_owned()),
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        });

        let state = infer_agent_state(&evidence, &[]);

        assert_eq!(state.agent.kind, AgentKind::Declared);
        assert_eq!(state.state, AgentState::Blocked);
        assert_eq!(
            format!("{:?}", state.attention),
            "High",
            "blocked derives high attention when none is declared"
        );
    }

    /// ADR-0040 compatibility path: no record ⇒ the title/screen heuristics
    /// behave exactly as before.
    #[test]
    fn absent_record_falls_back_to_heuristics() {
        let evidence = PaneEvidence::for_test(
            "@7",
            Some("phux-ask[deploy]:Approve deploy??s=Yes|No"),
            &["Codex is waiting"],
        );

        let state = infer_agent_state(&evidence, &[]);

        assert_eq!(state.agent.kind, AgentKind::Codex);
        assert_eq!(state.state, AgentState::Blocked);
        assert_eq!(state.sources[0].kind, "title_ask");
    }

    #[test]
    fn json_contains_confidence_and_sources() {
        let evidence = PaneEvidence::for_test("@9", Some("codex"), &["building"]);
        let state = infer_agent_state(&evidence, &[]);

        let value = serde_json::to_value(&state).expect("serialize state");

        assert_eq!(value["agent"]["id"], "codex");
        assert!(value["confidence"].is_number());
        assert_eq!(value["sources"][0]["kind"], "identity");
    }
}
