use super::infer_agent_state;
use super::{AgentKind, PaneEvidence, SessionEvidence};
use crate::agent_meta::{AgentMetaState, AgentRecord};

/// The committed golden the server detector is pinned against, so the
/// provenance test exercises the shipped manifest.
const CLAUDE_BLOCKED: &str =
    include_str!("../../../../phux-agent-rules/src/fixtures/claude/blocked_permission.txt");

fn record(name: &str, kind: &str, state: AgentMetaState) -> AgentRecord {
    AgentRecord {
        name: name.to_owned(),
        kind: Some(kind.to_owned()),
        state,
        ..AgentRecord::default()
    }
}

fn claude_blocked_pane() -> PaneEvidence {
    let lines: Vec<&str> = CLAUDE_BLOCKED.lines().collect();
    PaneEvidence::for_test("@4", None, &lines)
}

/// ADR-0103: with a live session under the pane, `agent show` names the
/// `stream` source and its state — including when the record disagrees.
#[test]
fn a_live_session_is_named_as_a_stream_source_even_when_it_disagrees() {
    for (facet, declared, agrees) in [
        (AgentMetaState::Working, AgentMetaState::Working, true),
        (AgentMetaState::Working, AgentMetaState::Idle, false),
    ] {
        let mut evidence = PaneEvidence::for_test("@7", Some("Claude Code"), &[""]);
        evidence.record = Some(record("worker", "claude", declared));
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
    evidence.record = Some(record("Reviewer", "claude", AgentMetaState::Working));

    let state = infer_agent_state(&evidence, &[]);

    assert_eq!(state.agent.kind, AgentKind::Claude);
    assert_eq!(state.agent.label, "Reviewer");
    assert_eq!(state.state, AgentMetaState::Working);
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
            AgentMetaState::Unknown,
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
    evidence.record = Some(record("claude", "claude", AgentMetaState::Blocked));

    let state = infer_agent_state(&evidence, &[]);

    assert_eq!(state.state, AgentMetaState::Blocked);
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
    evidence.record = Some(record("herdr-worker", "herdr", AgentMetaState::Blocked));

    let state = infer_agent_state(&evidence, &[]);

    assert_eq!(state.agent.kind, AgentKind::Declared);
    assert_eq!(state.state, AgentMetaState::Blocked);
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
        assert_eq!(report.state, AgentMetaState::Unknown);
    }
    for title in ["example project", "grokking Rust", "cursor-agentic"] {
        let evidence = PaneEvidence::for_test("@6", Some(title), &[]);
        assert_eq!(
            infer_agent_state(&evidence, &[]).agent.kind,
            AgentKind::Unknown
        );
    }
}

/// Every shipped detector kind keeps its own CLI identity, JSON kind, the
/// record's label, and the live title.
#[test]
fn shipped_detector_kinds_have_first_class_cli_identities() {
    for (slug, expected, json_kind) in [
        ("codex", AgentKind::Codex, "codex"),
        ("claude", AgentKind::Claude, "claude"),
        ("opencode", AgentKind::OpenCode, "open_code"),
        ("pi", AgentKind::Pi, "pi"),
        ("omp", AgentKind::Omp, "omp"),
        ("grok", AgentKind::Grok, "grok"),
        ("amp", AgentKind::Amp, "amp"),
        ("cursor-agent", AgentKind::CursorAgent, "cursor_agent"),
        ("gemini", AgentKind::Gemini, "gemini"),
        ("goose", AgentKind::Goose, "goose"),
        ("aider", AgentKind::Aider, "aider"),
    ] {
        let mut evidence = PaneEvidence::for_test("@6", Some("live program title"), &[]);
        evidence.record = Some(record("worker", slug, AgentMetaState::Working));
        let report = infer_agent_state(&evidence, &[]);
        assert_eq!(report.agent.kind, expected, "{slug}");
        assert_eq!(report.state, AgentMetaState::Working);
        let json = serde_json::to_value(&report).expect("serialize report");
        assert_eq!(json["agent"]["kind"], json_kind, "{slug}");
        assert_eq!(json["agent"]["id"], slug);
        assert_eq!(json["agent"]["label"], "worker");
        assert_eq!(json["title"], "live program title");
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
    assert_eq!(state.state, AgentMetaState::Blocked);
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

/// `agent list --json` spells `state` and `attention` in the shared
/// `phux.agent/v1` vocabulary, one lowercase word each.
#[test]
fn json_state_and_attention_words_are_pinned() {
    use crate::agent_meta::AgentAttention;
    for (state, word, attention) in [
        (AgentMetaState::Unknown, "unknown", "low"),
        (AgentMetaState::Idle, "idle", "none"),
        (AgentMetaState::Working, "working", "normal"),
        (AgentMetaState::Blocked, "blocked", "high"),
        (AgentMetaState::Done, "done", "low"),
    ] {
        let mut evidence = PaneEvidence::for_test("@4", None, &[]);
        evidence.record = Some(record("worker", "codex", state));
        let value = serde_json::to_value(infer_agent_state(&evidence, &[])).expect("serialize");
        assert_eq!(value["state"], word);
        assert_eq!(value["attention"], attention, "{word}");
    }
    for (attention, word) in [
        (AgentAttention::None, "none"),
        (AgentAttention::Low, "low"),
        (AgentAttention::Normal, "normal"),
        (AgentAttention::High, "high"),
    ] {
        let mut evidence = PaneEvidence::for_test("@4", None, &[]);
        let mut declared = record("worker", "codex", AgentMetaState::Working);
        declared.attention = Some(attention);
        evidence.record = Some(declared);
        let value = serde_json::to_value(infer_agent_state(&evidence, &[])).expect("serialize");
        assert_eq!(value["attention"], word);
    }
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

/// A shell in the foreground runs no agent: "claude" in its prompt or in
/// output it printed is text, not identity. Another foreground keeps the
/// screen heuristic.
#[test]
fn a_shell_foreground_outranks_agent_words_on_the_screen() {
    use crate::agent_meta::PaneOccupantRecord;
    let screen = ["host:claude-501 me$ cat notes", "codex and claude"];
    let mut evidence = PaneEvidence::for_test("@2", Some("claude-501"), &screen);
    evidence.occupant = Some(PaneOccupantRecord {
        foreground: "bash".to_owned(),
        is_pane_shell: true,
        background: false,
    });
    let state = infer_agent_state(&evidence, &[]);
    assert_eq!(state.agent.kind, AgentKind::Unknown);
    assert_eq!(state.sources[0].observed, "bash");

    evidence.occupant = Some(PaneOccupantRecord {
        foreground: "node".to_owned(),
        is_pane_shell: false,
        background: false,
    });
    assert_eq!(
        infer_agent_state(&evidence, &[]).agent.kind,
        AgentKind::Codex
    );
}

/// A name `%` cannot use prints the command that names the pane; a partial
/// view keeps its code.
#[test]
fn an_unaddressable_name_prints_the_command_that_names_the_pane() {
    let mut evidence = PaneEvidence::for_test("@3", Some("OpenCode"), &["some screen"]);
    evidence.record = Some(record("Code Review", "opencode", AgentMetaState::Blocked));
    let mut report = infer_agent_state(&evidence, &[]);
    for code in ["invalid_agent_name", "selector_not_single"] {
        report.set_address(Some(Err(code)));
        assert_eq!(
            report.address_column(),
            "(name it: phux agent set @3 --name NAME)"
        );
    }
    report.set_address(Some(Err("partial_view")));
    assert_eq!(report.address_column(), "(not %-addressable: partial_view)");
    report.set_address(Some(Ok("%opencode-2".to_owned())));
    assert_eq!(report.address_column(), "%opencode-2");
}
