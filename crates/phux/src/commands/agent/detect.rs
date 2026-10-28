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
//! This is a LEVEL read (`docs/program-sequences.md#the-agent-record`): nothing here is evidence
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
        "gemini" => AgentKind::Gemini,
        "goose" => AgentKind::Goose,
        "aider" => AgentKind::Aider,
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
        address: None,
        address_refusal: None,
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
        address: None,
        address_refusal: None,
    }
}

/// Identity, which is a separate question from state and is allowed to be a
/// guess: mislabelling a pane costs a wrong name, not a wrong gate.
fn infer_identity(
    evidence: &PaneEvidence,
    plugins: &[PluginAgent],
    sources: &mut Vec<AgentSource>,
) -> AgentIdentity {
    // Screen and title words are a guess about what runs; the occupant is
    // the server's observation of it. `cat README` or a cwd named
    // `claude-501` in a bash prompt is text, not a Claude.
    if let Some(occupant) = evidence.occupant.as_ref().filter(|o| o.is_pane_shell) {
        sources.push(AgentSource::new(
            "identity",
            "foreground is the pane shell",
            0.9,
            occupant.foreground.clone(),
        ));
        return identity("unknown", "Unknown agent", AgentKind::Unknown);
    }
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
mod tests;
