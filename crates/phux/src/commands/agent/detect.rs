//! The client-side projection behind `phux agent list` / `show` / `explain`.
//!
//! Agent state has one authority: the server's detector, published as the
//! pane's `phux.agent/v1` record (ADR-0046, ADR-0040), which `agent wait`
//! also reads. This module reports that record — it never re-derives state
//! from screen text. The manifest is replayed against the current screen only
//! to name the rule and region behind the state. With no record, state is
//! `unknown`, except for two declarations: the ADR-0035 `phux-ask` title
//! sentinel and a `[[plugins]]` agent declaration.
//!
//! This is a LEVEL read (`docs/spec/L3.md` §3.7): nothing here is evidence
//! that a turn finished. The completion gate is `phux agent wait`.

use phux_agent_rules::explain::{
    self as agent_explain, Capture, EvaluatedRule, Explanation, PredicateEvidence,
};
use phux_client::agent_meta::{AgentMetaState, AgentRecord};

use super::model::{
    AgentIdentity, AgentKind, AgentSource, AgentState, AgentStateReport, PaneEvidence, PluginAgent,
    StateSignal, attention_for, identity, plugin_attention, record_attention, record_state,
};

pub(super) fn infer_agent_state(
    evidence: &PaneEvidence,
    plugins: &[PluginAgent],
) -> AgentStateReport {
    if let Some(record) = &evidence.record {
        return report_from_record(evidence, plugins, record);
    }
    report_without_record(evidence, plugins)
}

/// Report the pane's published `phux.agent/v1` record, with the detector's
/// own evidence carried alongside it.
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
        "grok" => AgentKind::Grok,
        "amp" => AgentKind::Amp,
        "cursor-agent" => AgentKind::CursorAgent,
        other if plugins.iter().any(|plugin| plugin.id == other) => AgentKind::Plugin,
        _ => AgentKind::Declared,
    };
    let state = record_state(record.state);

    let mut sources = vec![AgentSource::new(
        "agent_record",
        "phux.agent/v1 record published for this pane",
        1.0,
        String::from_utf8(record.encode()).unwrap_or_default(),
    )];
    // ADR-0103: a session stream that decided this state is the top rung.
    if let Some(stream) = stream_source(evidence, state) {
        sources.insert(0, stream);
    }
    let explanation = match detector_trace(&slug, evidence, state) {
        Some(trace) => {
            sources.extend(trace.sources);
            trace.explanation
        }
        None => format!(
            "the phux.agent/v1 record for this pane (ADR-0040); no detection manifest is \
             loaded for `{slug}`, so there is no rule evidence to show"
        ),
    };
    sources.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));

    AgentStateReport {
        terminal: evidence.terminal.clone(),
        session: evidence.session.clone(),
        window: evidence.window.clone(),
        agent: identity(&slug, &record.name, kind),
        state,
        confidence: if state == AgentState::Unknown {
            0.3
        } else {
            1.0
        },
        attention: record_attention(record.effective_attention()),
        title: evidence.title.clone(),
        cwd: evidence.cwd.clone(),
        sources,
        explanation,
        agent_session: evidence
            .agent_session
            .as_ref()
            .map(super::model::SessionEvidence::to_json),
    }
}

/// The `stream` source: the pane's agent session derived a state from its
/// own event stream (ADR-0103). `None` only when there is no session or it has
/// not spoken. A disagreement with the record is surfaced, not withheld, but
/// demoted so the record still sorts first.
fn stream_source(evidence: &PaneEvidence, reported: AgentState) -> Option<AgentSource> {
    let session = evidence.agent_session.as_ref()?;
    if session.state == AgentMetaState::Unknown {
        return None;
    }
    let derived = record_state(session.state);
    let (why, confidence) = if derived == reported {
        (
            format!(
                "agent session {} ({}) derived the state from its event stream",
                session.resource, session.provider
            ),
            1.0,
        )
    } else {
        (
            format!(
                "agent session {} ({}) last derived '{}' from its event stream, which is not \
                 the state reported here",
                session.resource,
                session.provider,
                derived.as_str()
            ),
            0.5,
        )
    };
    Some(AgentSource::new(
        "stream",
        why,
        confidence,
        session.state.as_str(),
    ))
}

/// The evidence trail reconstructed from the ADR-0046 manifest.
#[derive(Debug)]
struct DetectorTrace {
    sources: Vec<AgentSource>,
    explanation: String,
}

/// Replay the detection manifest for `slug` against the screen this
/// projection already read, reporting the **rule**, never the state. The
/// record stays authoritative; a disagreement (the screen moved on since the
/// record was published) is surfaced in the explanation.
fn detector_trace(
    slug: &str,
    evidence: &PaneEvidence,
    reported: AgentState,
) -> Option<DetectorTrace> {
    let kind = agent_explain::resolve_kind(slug)?;
    let capture = Capture {
        title: evidence.title.clone().unwrap_or_default(),
        lines: evidence.lines.clone(),
    };
    let explained = agent_explain::explain(&kind, &capture)?;

    let matched = explained
        .matched_rule
        .as_deref()
        .and_then(|id| find_rule(&explained, id));

    let mut sources = Vec::new();
    let explanation = if let Some(rule) = matched {
        let asserted = rule.state.as_deref().unwrap_or("no state");
        sources.push(
            AgentSource::new(
                "detector_rule",
                format!("ADR-0046 rule `{}` asserts {asserted}", rule.id),
                0.9,
                matched_patterns(&rule.evidence).join(" & "),
            )
            .with_rule(rule.id.clone(), rule.region.clone()),
        );
        if explained.detector_state == reported.as_str() {
            format!(
                "the phux.agent/v1 record for this pane (ADR-0040), derived by the `{kind}` \
                 manifest: rule `{}` matched the `{}` region",
                rule.id, rule.region,
            )
        } else {
            format!(
                "the phux.agent/v1 record for this pane (ADR-0040); the `{kind}` manifest \
                 reads this screen as '{}' now, via rule `{}` on the `{}` region — the \
                 screen moved on after the record was published",
                explained.detector_state, rule.id, rule.region,
            )
        }
    } else {
        let reason = explained.fallback_reason.clone().unwrap_or_else(|| {
            "no state-bearing rule matched this screen; the detector fails safe to idle".to_owned()
        });
        sources.push(AgentSource::new(
            "detector_fallback",
            format!("no `{kind}` rule matches this screen"),
            0.3,
            reason.clone(),
        ));
        format!(
            "the phux.agent/v1 record for this pane (ADR-0040); nothing in the `{kind}` \
             manifest matches this screen now ({reason})"
        )
    };
    for flag in positive_flags(&explained) {
        sources.push(AgentSource::new(
            "detector_flag",
            format!("a matching `{kind}` rule asserts {flag}"),
            0.6,
            flag,
        ));
    }
    Some(DetectorTrace {
        sources,
        explanation,
    })
}

fn find_rule<'a>(explained: &'a Explanation, id: &str) -> Option<&'a EvaluatedRule> {
    explained.evaluated_rules.iter().find(|rule| rule.id == id)
}

/// The patterns that actually matched, so `observed` shows the text the rule
/// saw rather than restating the rule's own name.
fn matched_patterns(node: &PredicateEvidence) -> Vec<String> {
    let mut out = Vec::new();
    collect_matched(node, &mut out);
    if out.is_empty() {
        out.push("matched with no literal pattern".to_owned());
    }
    out
}

fn collect_matched(node: &PredicateEvidence, out: &mut Vec<String>) {
    if node.matched
        && let Some(pattern) = &node.pattern
    {
        out.push(format!("{} {pattern:?}", node.op));
    }
    for child in &node.children {
        collect_matched(child, out);
    }
}

pub(super) fn positive_flags(explained: &Explanation) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if explained.visible_idle {
        flags.push("visible-idle");
    }
    if explained.freeze {
        flags.push("skip-state-update");
    }
    flags
}

/// No `phux.agent/v1` record: no derived lifecycle state either. Only a live
/// session stream, the `phux-ask` title sentinel, or a `[[plugins]]`
/// declaration carries state; everything else is evidence.
fn report_without_record(evidence: &PaneEvidence, plugins: &[PluginAgent]) -> AgentStateReport {
    let mut sources = Vec::new();
    let agent = infer_identity(evidence, plugins, &mut sources);
    let plugin = plugins.iter().find(|plugin| plugin.id == agent.id);

    let declared = declared_state(evidence, &mut sources);
    // ADR-0103: a live session's derived state outranks everything below.
    let stream_state = evidence
        .agent_session
        .as_ref()
        .filter(|session| session.state != AgentMetaState::Unknown)
        .map(|session| record_state(session.state));
    let mut state = stream_state.map_or(declared, |derived| {
        if let Some(stream) = stream_source(evidence, derived) {
            sources.push(stream);
        }
        StateSignal::new(
            derived,
            1.0,
            "the agent session's own event stream reported this state",
        )
    });
    if let Some(plugin) = plugin {
        sources.push(AgentSource::new(
            "plugin_report",
            "configured agent declaration",
            0.55,
            format!("{} reports {:?}", plugin.id, plugin.state),
        ));
        if state.state == AgentState::Unknown {
            state = StateSignal::from_plugin(plugin.state);
        }
    }
    if state.state == AgentState::Unknown {
        sources.push(AgentSource::new(
            "no_agent_record",
            "no phux.agent/v1 record published for this pane",
            0.2,
            "the server publishes agent state; this pane has none",
        ));
    }
    // Screen evidence, reported and never decisive (phux-w7z2.31).
    if evidence.semantic_input {
        sources.push(AgentSource::new(
            "semantic_cells",
            "OSC-133 input cells on screen (evidence only)",
            0.1,
            "input region",
        ));
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
        agent_session: evidence
            .agent_session
            .as_ref()
            .map(super::model::SessionEvidence::to_json),
    }
}

/// Identity, which is a separate question from state and is allowed to be a
/// guess: mislabelling a pane costs a wrong name, not a wrong gate.
fn infer_identity(
    evidence: &PaneEvidence,
    plugins: &[PluginAgent],
    sources: &mut Vec<AgentSource>,
) -> AgentIdentity {
    let text = evidence_text(evidence);
    if contains_token(&text, "codex") {
        sources.push(AgentSource::new("identity", "codex marker", 0.8, "Codex"));
        return identity("codex", "Codex", AgentKind::Codex);
    }
    if contains_token(&text, "claude") {
        sources.push(AgentSource::new("identity", "claude marker", 0.8, "Claude"));
        return identity("claude", "Claude", AgentKind::Claude);
    }
    if let Some(agent) = infer_captured_title_identity(evidence) {
        sources.push(AgentSource::new(
            "identity",
            "agent title marker",
            0.8,
            agent.label.clone(),
        ));
        return agent;
    }
    for plugin in plugins {
        if contains_token(&text, &plugin.id) || contains_token(&text, &plugin.label) {
            sources.push(AgentSource::new(
                "identity",
                "plugin marker",
                0.65,
                plugin.label.clone(),
            ));
            return identity(&plugin.id, &plugin.label, AgentKind::Plugin);
        }
    }
    identity("unknown", "Unknown agent", AgentKind::Unknown)
}

/// Recognize the additional captured agent titles as identity evidence only.
/// Whole words avoid treating "example" as Amp or "grokking" as Grok.
fn infer_captured_title_identity(evidence: &PaneEvidence) -> Option<AgentIdentity> {
    let title = evidence.title.as_deref()?.to_lowercase();
    let words: Vec<_> = title
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    for (slug, label, kind, marker) in [
        ("grok", "Grok", AgentKind::Grok, &["grok"][..]),
        ("amp", "Amp", AgentKind::Amp, &["amp"][..]),
        (
            "cursor-agent",
            "Cursor Agent",
            AgentKind::CursorAgent,
            &["cursor", "agent"][..],
        ),
    ] {
        if words.windows(marker.len()).any(|window| window == marker) {
            return Some(identity(slug, label, kind));
        }
    }
    None
}

/// The one screen-borne state signal that is a *declaration*: the ADR-0035
/// `phux-ask` title sentinel.
fn declared_state(evidence: &PaneEvidence, sources: &mut Vec<AgentSource>) -> StateSignal {
    if let Some(title) = evidence.title.as_deref()
        && title.starts_with("phux-ask")
        && title.contains(':')
    {
        sources.push(AgentSource::new(
            "title_ask",
            "phux-ask title sentinel",
            0.95,
            title,
        ));
        return StateSignal::new(
            AgentState::Blocked,
            0.95,
            "waiting on a reported human-answerable ask (ADR-0035 title sentinel)",
        );
    }
    StateSignal::new(
        AgentState::Unknown,
        0.2,
        "no phux.agent/v1 record: the server has published no agent state for this pane",
    )
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

#[cfg(test)]
mod tests {
    use super::infer_agent_state;
    use crate::commands::agent::model::{AgentKind, AgentState, PaneEvidence, SessionEvidence};
    use phux_client::agent_meta::{AgentMetaState, AgentRecord};

    /// The committed golden the server detector is pinned against, so the
    /// provenance test exercises the shipped manifest.
    const CLAUDE_BLOCKED: &str =
        include_str!("../../../../phux-agent-rules/src/fixtures/claude/blocked_permission.txt");

    fn claude_blocked_pane() -> PaneEvidence {
        let lines: Vec<&str> = CLAUDE_BLOCKED.lines().collect();
        PaneEvidence::for_test("@4", None, &lines)
    }

    /// ADR-0103: with a live session under the pane, `agent show` names the
    /// `stream` source and its state — including when the record disagrees.
    #[test]
    fn a_live_session_is_named_as_a_stream_source_even_when_it_disagrees() {
        for (facet, record, agrees) in [
            (AgentMetaState::Working, AgentMetaState::Working, true),
            (AgentMetaState::Working, AgentMetaState::Idle, false),
        ] {
            let mut evidence = PaneEvidence::for_test("@7", Some("Claude Code"), &[""]);
            evidence.record = Some(AgentRecord {
                name: "worker".to_owned(),
                kind: Some("claude".to_owned()),
                state: record,
                ..AgentRecord::default()
            });
            evidence.agent_session = Some(SessionEvidence {
                resource: "@8".to_owned(),
                provider: "claude".to_owned(),
                native_id: None,
                state: facet,
            });

            let report = infer_agent_state(&evidence, &[]);
            let stream = report
                .sources
                .iter()
                .find(|source| source.kind == "stream")
                .unwrap_or_else(|| panic!("a live session must be named: {:?}", report.sources));
            assert_eq!(
                stream.observed,
                facet.as_str(),
                "the stream source carries the state the stream derived",
            );
            assert_eq!(
                report.sources[0].kind,
                if agrees { "stream" } else { "agent_record" },
                "the record stays authoritative when the two disagree",
            );
        }
    }

    /// ADR-0040: a declared record outranks every other signal — the title is
    /// a `phux-ask` sentinel AND the screen screams "codex blocked", but the
    /// structured record says a working Claude and that is what reports.
    #[test]
    fn declared_record_outranks_the_title_sentinel_and_the_screen() {
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
        assert_eq!(
            state.sources[0].kind, "agent_record",
            "the record is the top-ranked source"
        );
        assert!(
            !state
                .sources
                .iter()
                .any(|source| source.kind == "title_ask"),
            "no declaration may compete with the record"
        );
    }

    /// `phux agent wait` reads the published record; this projection must
    /// report exactly the same state, whatever the screen says.
    #[test]
    fn the_projection_reports_the_records_state_verbatim() {
        for meta in [
            AgentMetaState::Idle,
            AgentMetaState::Working,
            AgentMetaState::Blocked,
            AgentMetaState::Done,
        ] {
            let mut evidence = PaneEvidence::for_test(
                "@6",
                Some("Claude Code"),
                &["do you want to continue? need approval"],
            );
            evidence.record = Some(AgentRecord {
                name: "worker".to_owned(),
                kind: Some("claude".to_owned()),
                state: meta,
                ..AgentRecord::default()
            });

            let state = infer_agent_state(&evidence, &[]);

            assert_eq!(
                state.state.as_str(),
                meta.as_str(),
                "record state {meta:?} must survive the projection"
            );
        }
    }

    /// With no record the listing asserts no state, however loudly the screen
    /// suggests one (`agent wait` refuses the pane with `no_agent_record`).
    #[test]
    fn without_a_record_the_projection_asserts_no_state() {
        for screen in [
            &["need approval to continue?"][..],
            &["all tasks complete", "tests passed"][..],
            &["thinking...", "compiling"][..],
            &["$ "][..],
        ] {
            let evidence = PaneEvidence::for_test("@7", Some("claude"), screen);
            let state = infer_agent_state(&evidence, &[]);

            assert_eq!(
                state.state,
                AgentState::Unknown,
                "screen {screen:?} must not produce a state"
            );
            assert!(
                state
                    .sources
                    .iter()
                    .any(|source| source.kind == "no_agent_record"),
                "the absence has to be reported, not implied: {:?}",
                state.sources
            );
        }
    }

    /// A record-backed state carries the ADR-0046 rule id and region that
    /// produced it, against the shipped Claude manifest.
    #[test]
    fn record_backed_state_carries_the_detector_rule_id_and_region() {
        let mut evidence = claude_blocked_pane();
        evidence.record = Some(AgentRecord {
            name: "claude".to_owned(),
            kind: Some("claude".to_owned()),
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        });

        let state = infer_agent_state(&evidence, &[]);

        assert_eq!(state.state, AgentState::Blocked);
        let rule = state
            .sources
            .iter()
            .find(|source| source.kind == "detector_rule")
            .expect("the blocked permission fixture must match a shipped rule");
        assert!(rule.rule.is_some(), "the rule id is the provenance");
        assert!(rule.region.is_some(), "the region is half the provenance");
        assert!(
            state.explanation.contains("ADR-0040"),
            "{}",
            state.explanation
        );
    }

    /// A record whose kind has no manifest still reports the record, with the
    /// absence of rule evidence stated (and no stream source without a session).
    #[test]
    fn a_record_with_no_manifest_reports_the_record_and_says_why_there_is_no_rule() {
        let mut evidence = PaneEvidence::for_test("@9", None, &["some screen"]);
        evidence.record = Some(AgentRecord {
            name: "herdr-worker".to_owned(),
            kind: Some("herdr".to_owned()),
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        });

        let state = infer_agent_state(&evidence, &[]);

        assert_eq!(state.agent.kind, AgentKind::Declared);
        assert_eq!(state.state, AgentState::Blocked);
        assert_eq!(format!("{:?}", state.attention), "High");
        assert_eq!(state.sources.len(), 1);
        assert_eq!(state.sources[0].kind, "agent_record");
        assert!(
            state.explanation.contains("no detection manifest"),
            "{}",
            state.explanation
        );
    }

    #[test]
    fn captured_agent_kinds_keep_first_class_json_identity_and_live_title() {
        for (slug, json_kind) in [
            ("grok", "grok"),
            ("amp", "amp"),
            ("cursor-agent", "cursor_agent"),
        ] {
            let mut evidence = PaneEvidence::for_test("@6", Some("live program title"), &[]);
            evidence.record = Some(AgentRecord {
                name: "worker".to_owned(),
                kind: Some(slug.to_owned()),
                state: AgentMetaState::Working,
                ..AgentRecord::default()
            });
            let report = infer_agent_state(&evidence, &[]);
            let json = serde_json::to_value(&report).expect("serialize report");
            assert_eq!(json["agent"]["kind"], json_kind, "{slug}");
            assert_eq!(json["agent"]["id"], slug);
            assert_eq!(json["agent"]["label"], "worker");
            assert_eq!(json["title"], "live program title");
            assert_eq!(report.state, AgentState::Working);
        }
    }

    #[test]
    fn captured_agent_titles_are_identity_evidence_not_state_declarations() {
        for (title, slug, json_kind) in [
            ("Thinking - grok", "grok", "grok"),
            ("Amp", "amp", "amp"),
            ("Cursor Agent", "cursor-agent", "cursor_agent"),
            ("cursor-agent", "cursor-agent", "cursor_agent"),
        ] {
            let evidence = PaneEvidence::for_test("@6", Some(title), &[]);
            let report = infer_agent_state(&evidence, &[]);
            let json = serde_json::to_value(&report).expect("serialize report");
            assert_eq!(json["agent"]["id"], slug, "{title}");
            assert_eq!(json["agent"]["kind"], json_kind, "{title}");
            assert_eq!(report.state, AgentState::Unknown);
        }
        for title in ["example project", "grokking Rust", "cursor-agentic"] {
            let evidence = PaneEvidence::for_test("@6", Some(title), &[]);
            assert_eq!(
                infer_agent_state(&evidence, &[]).agent.kind,
                AgentKind::Unknown
            );
        }
    }

    #[test]
    fn shipped_detector_kinds_have_first_class_cli_identities() {
        for (slug, expected) in [
            ("codex", AgentKind::Codex),
            ("claude", AgentKind::Claude),
            ("opencode", AgentKind::OpenCode),
            ("pi", AgentKind::Pi),
            ("omp", AgentKind::Omp),
            ("grok", AgentKind::Grok),
            ("amp", AgentKind::Amp),
            ("cursor-agent", AgentKind::CursorAgent),
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

    /// ADR-0035: the `phux-ask` sentinel survives as a state source on a pane
    /// with no record, because the agent declared it about itself over a
    /// normative escape sequence rather than a classifier inferring it.
    #[test]
    fn the_ask_sentinel_still_reports_blocked_without_a_record() {
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
        // `rule` / `region` are absent on a source that is not a rule, so a
        // consumer probes by presence rather than for a sentinel.
        assert!(value["sources"][0].get("rule").is_none());
    }

    /// A confidence reaches JSON as the decimal it was written as, not the
    /// widened `f64` noise of an `f32` (`0.20000000298023224`).
    #[test]
    fn json_confidence_is_the_shortest_decimal() {
        let evidence = PaneEvidence::for_test("@9", None, &[]);
        let state = infer_agent_state(&evidence, &[]);
        assert_eq!(state.confidence, 0.2_f32, "the unknown-pane confidence");

        let rendered =
            serde_json::to_string(&serde_json::to_value(&state).expect("value")).expect("render");
        assert!(rendered.contains("\"confidence\":0.2,"), "{rendered}");
        assert!(!rendered.contains("0.2000000"), "{rendered}");
    }
}
