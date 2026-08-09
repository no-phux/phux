//! The client-side projection behind `phux agent list` / `show` / `explain`.
//!
//! # One detector, not two (phux-w7z2.31)
//!
//! Agent state has exactly one authority: the server's level-triggered
//! per-terminal detector, which evaluates region-scoped TOML rules
//! (`crates/phux-server/rules/*.toml`) and publishes the result as the pane's
//! `phux.agent/v1` L3 record (ADR-0046, ADR-0040). `phux agent wait` reads
//! that record. This module is the *projection* of the same record for the
//! listing verbs, and its job is to report it — not to re-derive it.
//!
//! It used to re-derive it. For any pane the detector had not published — a
//! kind with no manifest, a pane still inside the detector's settle window,
//! a machine with `PHUX_AGENT_DETECT=0` — this file ran a client-side
//! classifier over the pane's scrollback and asserted `blocked` from the word
//! "permission", `done` from the word "complete", `working` from the word
//! "thinking". Two verbs in one binary answered the same question differently,
//! and the half that was wrong was the one an orchestrator polls most. Worse,
//! it contradicted ADR-0046's fail-safe in the one direction that ADR forbids:
//! the detector never invents `blocked`, and this did, from a substring.
//!
//! So screen content no longer *decides* anything here. It is still read, and
//! still reported, but only as evidence:
//!
//! - a published `phux.agent/v1` record is the state, always;
//! - alongside it, the ADR-0046 manifest is replayed against the pane's
//!   current screen so `sources[]` carries the **rule id and region** behind
//!   the state, which is the provenance `agent explain --file` previously kept
//!   to itself;
//! - with no record, the projection asserts no lifecycle state. `unknown` is
//!   the honest answer, and it is the same answer `agent wait` gives that pane
//!   by refusing with `no_agent_record`.
//!
//! Two declarations survive as state sources because neither is a derivation:
//! the ADR-0035 `phux-ask` title sentinel, which an agent writes about itself
//! over a normative escape sequence, and a `[[plugins]]` agent declaration,
//! which an operator writes in their own config.
//!
//! # This is a LEVEL read
//!
//! Everything here answers "what is true of this pane right now"
//! (`docs/spec/L3.md` §3.7). A level read asserts only the absence of contrary
//! evidence, which is equally true of a crashed pane, so **no output of this
//! module is evidence that a turn finished**. The completion gate is
//! `phux agent wait`, which requires an observed transition. `agent list`
//! showing `idle` is a listing, not a receipt.

use phux_client::agent_meta::AgentRecord;
use phux_server::agent_explain::{self, Capture, EvaluatedRule, Explanation, PredicateEvidence};

use super::model::{
    AgentIdentity, AgentKind, AgentSource, AgentState, AgentStateReport, PaneEvidence, PluginAgent,
    StateSignal, attention_for, identity, plugin_attention, record_attention, record_state,
};

pub(super) fn infer_agent_state(
    evidence: &PaneEvidence,
    plugins: &[PluginAgent],
) -> AgentStateReport {
    // ADR-0040 / ADR-0046: the published record is the server's answer, and
    // the server is the authority. Nothing below overrides it.
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
        // A published record is a report, not a guess. The one case that is
        // not certain is a record whose state was withdrawn to `unknown`.
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
    }
}

/// The evidence trail reconstructed from the ADR-0046 manifest.
#[derive(Debug)]
struct DetectorTrace {
    sources: Vec<AgentSource>,
    explanation: String,
}

/// Replay the detection manifest for `slug` against the screen this
/// projection already read, and report what it says.
///
/// The rules engine is compiled into this binary — `phux_server::agent_explain`
/// is the same facade `agent explain --file` runs offline — so the manifest
/// that produced the server's record can be evaluated here with no extra
/// round trip and no second implementation of the matching.
///
/// It reports the **rule**, never the state. The record stays authoritative
/// even when this replay disagrees with it, and the two legitimately disagree:
/// the record was published when the server last scanned, this reads the
/// screen as of the snapshot the listing fetched, and in between the agent may
/// have repainted. A disagreement is therefore surfaced in the explanation
/// rather than resolved, because "the screen has moved on since the record was
/// published" is exactly the thing a caller wants told, not hidden.
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

fn positive_flags(explained: &Explanation) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if explained.visible_blocker {
        flags.push("visible-blocker");
    }
    if explained.visible_idle {
        flags.push("visible-idle");
    }
    if explained.visible_working {
        flags.push("visible-working");
    }
    if explained.freeze {
        flags.push("skip-state-update");
    }
    flags
}

/// No `phux.agent/v1` record: the server has published nothing about this
/// pane, so this projection publishes no derived lifecycle state either.
///
/// Two declarations still carry state, because neither is a re-derivation of
/// the detector's job:
///
/// 1. an ADR-0035 `phux-ask` title sentinel — the agent saying, over a
///    normative phux escape sequence, that it is waiting on a human answer;
/// 2. a `[[plugins]]` agent declaration from the operator's own config.
///
/// Everything else the pane shows is reported as evidence with no state
/// attached. That is the honest answer, and it is the answer `agent wait`
/// gives the same pane when it refuses with `no_agent_record`.
fn report_without_record(evidence: &PaneEvidence, plugins: &[PluginAgent]) -> AgentStateReport {
    let mut sources = Vec::new();
    let agent = infer_identity(evidence, plugins, &mut sources);
    let plugin = plugins.iter().find(|plugin| plugin.id == agent.id);

    let mut state = declared_state(evidence, &mut sources);
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

/// The one screen-borne state signal that is a *declaration* rather than a
/// derivation: the ADR-0035 `phux-ask` title sentinel, which the server
/// already parses into an `Asked` event and which the keybinding live-feed
/// treats the same way (`commands/config/live_feed.rs`).
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
    use crate::commands::agent::model::{AgentKind, AgentState, PaneEvidence};
    use phux_client::agent_meta::{AgentMetaState, AgentRecord};

    /// A REAL committed golden: the Claude Code permission dialog the server
    /// detector is pinned against. Using it here is what makes the rule-id
    /// provenance test a test of the shipped manifest rather than of a screen
    /// invented to match it (the exact failure ADR-0046 records).
    const CLAUDE_BLOCKED: &str = include_str!(
        "../../../../phux-server/src/agent_detect/fixtures/claude/blocked_permission.txt"
    );

    fn claude_blocked_pane() -> PaneEvidence {
        let lines: Vec<&str> = CLAUDE_BLOCKED.lines().collect();
        PaneEvidence::for_test("@4", None, &lines)
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

    /// phux-w7z2.31, the divergence itself. `phux agent wait` reads the
    /// published record; this projection must report exactly the same state
    /// for the same pane, for every state in the vocabulary.
    #[test]
    fn the_projection_reports_the_records_state_verbatim() {
        for meta in [
            AgentMetaState::Idle,
            AgentMetaState::Working,
            AgentMetaState::Blocked,
            AgentMetaState::Done,
        ] {
            // A screen that the old client classifier read as `blocked`, to
            // prove it can no longer override the record in either direction.
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

    /// phux-w7z2.31, the other half. With no record the server has published
    /// nothing, and `agent wait` refuses that pane with `no_agent_record`. The
    /// listing must not answer a question the server declined to answer —
    /// however loudly the screen suggests one.
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

    /// The acceptance criterion, stated as one assertion: for the same pane,
    /// the state this projection reports and the state `agent wait`'s
    /// predicate reads are the same value — because they are the same value.
    #[test]
    fn agent_list_and_agent_wait_cannot_disagree() {
        let mut evidence = PaneEvidence::for_test("@8", Some("Claude Code"), &["working away"]);
        let record = AgentRecord {
            name: "worker".to_owned(),
            kind: Some("claude".to_owned()),
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        };
        evidence.record = Some(record.clone());

        let projected = infer_agent_state(&evidence, &[]);

        // `phux agent wait` reads `record.state` off the same record; the
        // projection's mapping is the identity on that vocabulary.
        assert_eq!(projected.state.as_str(), record.state.as_str());
    }

    /// phux-w7z2.31's provenance half, against the shipped Claude manifest:
    /// a record-backed state carries the ADR-0046 rule id and region that
    /// produced it, not a decorative single entry.
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

    /// A record whose kind has no manifest (16 kinds today) still reports the
    /// record, with the absence of rule evidence stated rather than faked.
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
    fn shipped_detector_kinds_have_first_class_cli_identities() {
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
}
