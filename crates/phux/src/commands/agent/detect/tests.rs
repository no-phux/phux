use super::infer_agent_state;
use crate::commands::agent::model::{AgentKind, AgentState, PaneEvidence, SessionEvidence};
use phux_client::agent_meta::{AgentMetaState, AgentRecord};

/// The committed golden the server detector is pinned against, so the
/// provenance test exercises the shipped manifest.
const CLAUDE_BLOCKED: &str =
    include_str!("../../../../../phux-agent-rules/src/fixtures/claude/blocked_permission.txt");

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
        ("gemini", "gemini"),
        ("goose", "goose"),
        ("aider", "aider"),
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
        ("gemini", AgentKind::Gemini),
        ("goose", AgentKind::Goose),
        ("aider", AgentKind::Aider),
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

/// A shell in the foreground runs no agent: "claude" in its prompt or in
/// output it printed is text, not identity. Another foreground keeps the
/// screen heuristic.
#[test]
fn a_shell_foreground_outranks_agent_words_on_the_screen() {
    use phux_client::agent_meta::PaneOccupantRecord;
    let screen = ["host:claude-501 me$ cat notes", "codex and claude"];
    let mut evidence = PaneEvidence::for_test("@2", Some("claude-501"), &screen);
    evidence.occupant = Some(PaneOccupantRecord {
        foreground: "bash".to_owned(),
        is_pane_shell: true,
    });
    let state = infer_agent_state(&evidence, &[]);
    assert_eq!(state.agent.kind, AgentKind::Unknown);
    assert_eq!(state.sources[0].observed, "bash");

    evidence.occupant = Some(PaneOccupantRecord {
        foreground: "node".to_owned(),
        is_pane_shell: false,
    });
    assert_eq!(
        infer_agent_state(&evidence, &[]).agent.kind,
        AgentKind::Codex
    );
}
