use super::*;
use crate::layout::{Group, LayoutNode, LayoutState, SplitDir, split_at};

fn tid(n: u32) -> ResourceId {
    ResourceId::local(n)
}

fn sinfo(id: u32, name: &str, windows: u16) -> SessionInfo {
    SessionInfo::new(SessionId::new(id), name).with_window_count(windows)
}

fn record(name: &str, kind: Option<&str>, state: AgentMetaState) -> AgentRecord {
    AgentRecord {
        name: name.to_owned(),
        kind: kind.map(ToOwned::to_owned),
        state,
        ..AgentRecord::default()
    }
}

/// Two windows: window 0 (`main`) split into panes `a`|`b`, window 1
/// (`logs`) a single pane `c`.
fn two_window_workspace_ids(a: u32, b: u32, c: u32) -> ArrangementDoc {
    let tree = split_at(
        &LayoutNode::Leaf(tid(a)),
        &tid(a),
        &tid(b),
        SplitDir::Horizontal,
        0.5,
    )
    .expect("split");
    ArrangementDoc {
        groups: vec![
            Group::new(
                "main".to_owned(),
                LayoutState {
                    tree: Some(tree),
                    focus: Some(tid(a)),
                },
            ),
            Group::new("logs".to_owned(), LayoutState::single(tid(c))),
        ],
        active: 0,
    }
}

/// Two windows: window 0 split into panes 1|2, window 1 a single pane 3.
/// Rows for `workspace` alone: no session graph, no foreign state.
fn local_items(
    workspace: &ArrangementDoc,
    agents: &HashMap<ResourceId, AgentRecord>,
    meta: &HashMap<ResourceId, FleetPaneMeta>,
) -> Vec<SelectItem> {
    fleet_items(
        workspace,
        &[],
        None,
        agents,
        meta,
        ForeignSessions {
            layouts: &HashMap::new(),
            agents: &HashMap::new(),
            terminals: &HashMap::new(),
        },
    )
}

fn two_window_workspace() -> ArrangementDoc {
    two_window_workspace_ids(1, 2, 3)
}

#[test]
fn groups_current_session_panes_under_header_and_foreign_as_switch_rows() {
    let workspace = two_window_workspace();
    let sessions = [sinfo(1, "work", 2), sinfo(2, "scratch", 3)];
    // No cached foreign layout: the single switch row, counting the
    // session's terminals from the graph.
    let items = fleet_items(
        &workspace,
        &sessions,
        Some(SessionId::new(1)),
        &HashMap::new(),
        &HashMap::new(),
        ForeignSessions {
            layouts: &HashMap::new(),
            agents: &HashMap::new(),
            terminals: &HashMap::from([(SessionId::new(2), 3)]),
        },
    );
    // Current session leads as a header, then one row per pane (3),
    // then the foreign session header + its switch row.
    assert!(items[0].is_header());
    assert_eq!(items[0].label, "work (current)");
    let pane_rows: Vec<&SelectItem> = items[1..4].iter().collect();
    assert!(pane_rows.iter().all(|i| !i.is_header() && i.indented));
    assert!(
        pane_rows.iter().all(|i| i.action.action == "focus-pane"),
        "current-session rows commit focus-pane"
    );
    assert!(items[4].is_header());
    assert_eq!(items[4].label, "scratch");
    assert_eq!(items[5].action.action, "switch-session");
    assert_eq!(
        items[5].action.args.get("name"),
        Some(&toml::Value::String("scratch".to_owned()))
    );
    assert_eq!(
        items[5].action.args.get("id"),
        Some(&toml::Value::Integer(2))
    );
    assert_eq!(items[5].secondary.as_deref(), Some("3 terminals"));
}

#[test]
fn pane_rows_carry_window_and_leaf_ordinals() {
    let workspace = two_window_workspace();
    let items = local_items(&workspace, &HashMap::new(), &HashMap::new());
    // Pre-snapshot fallback: flat pane rows, no headers.
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0].action.args.get("window"),
        Some(&toml::Value::Integer(0))
    );
    assert_eq!(
        items[0].action.args.get("pane"),
        Some(&toml::Value::Integer(0))
    );
    assert_eq!(
        items[1].action.args.get("pane"),
        Some(&toml::Value::Integer(1))
    );
    assert_eq!(
        items[2].action.args.get("window"),
        Some(&toml::Value::Integer(1))
    );
    assert_eq!(
        items[2].action.args.get("pane"),
        Some(&toml::Value::Integer(0))
    );
    // Labels carry the window:name.pane coordinates.
    assert!(items[0].label.contains("0:main.0"), "{}", items[0].label);
    assert!(items[2].label.contains("1:logs.0"), "{}", items[2].label);
}

#[test]
fn agent_record_supplies_name_kind_glyph_and_state_word() {
    let workspace = two_window_workspace();
    let mut agents = HashMap::new();
    agents.insert(
        tid(1),
        record("reviewer", Some("claude"), AgentMetaState::Working),
    );
    agents.insert(tid(2), record("builder", None, AgentMetaState::Blocked));
    let items = local_items(&workspace, &agents, &HashMap::new());
    assert_eq!(items[0].label, "◐ 0:main.0 reviewer [claude]");
    assert_eq!(items[0].secondary.as_deref(), Some("working"));
    assert!(!items[0].attention, "working is not high attention");
    assert_eq!(items[1].label, "● 0:main.1 builder");
    assert_eq!(items[1].secondary.as_deref(), Some("blocked"));
    assert!(
        items[1].attention,
        "blocked derives high attention (ADR-0040) and must highlight"
    );
}

/// An `AgentSession` stream bound to a pane supplies the row's state; the
/// `phux.agent/v1` record still names it. Two sessions under one pane are
/// two rows that both commit the same `focus-pane`.
#[test]
fn stream_sessions_outrank_the_record_and_list_one_row_each() {
    let workspace = ArrangementDoc::single(tid(1));
    let mut agents = HashMap::new();
    agents.insert(tid(1), record("reviewer", None, AgentMetaState::Idle));
    let mut meta = HashMap::new();
    meta.insert(
        tid(1),
        FleetPaneMeta {
            sessions: vec![
                AgentSessionRow {
                    id: tid(8),
                    provider: Some("claude".to_owned()),
                    native_id: None,
                    state: AgentMetaState::Blocked,
                },
                AgentSessionRow {
                    id: tid(9),
                    provider: None,
                    native_id: None,
                    state: AgentMetaState::Working,
                },
            ],
            ..FleetPaneMeta::default()
        },
    );
    let items = local_items(&workspace, &agents, &meta);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "● 0.0 reviewer [claude]");
    assert_eq!(items[0].secondary.as_deref(), Some("blocked"));
    assert!(items[0].attention, "a blocked stream highlights");
    assert_eq!(items[1].label, "◐ 0.0 reviewer");
    assert_eq!(items[1].secondary.as_deref(), Some("working"));
    assert!(!items[1].attention);
    assert!(
        items.iter().all(|i| i.action.action == "focus-pane"),
        "the parent pane is every session row's target"
    );
}

/// With no record the provider names the stream row.
#[test]
fn a_stream_without_a_record_is_named_by_its_provider() {
    let workspace = ArrangementDoc::single(tid(1));
    let mut meta = HashMap::new();
    meta.insert(
        tid(1),
        FleetPaneMeta {
            title: Some("some title".to_owned()),
            sessions: vec![AgentSessionRow {
                id: tid(8),
                provider: Some("codex".to_owned()),
                native_id: None,
                state: AgentMetaState::Done,
            }],
            ..FleetPaneMeta::default()
        },
    );
    let items = local_items(&workspace, &HashMap::new(), &meta);
    assert_eq!(items[0].label, "◆ 0.0 codex");
    assert_eq!(items[0].secondary.as_deref(), Some("done"));
}

#[test]
fn state_glyphs_cover_the_v1_vocabulary() {
    assert_eq!(agent_state_glyph(AgentMetaState::Blocked), "●");
    assert_eq!(agent_state_glyph(AgentMetaState::Working), "◐");
    assert_eq!(agent_state_glyph(AgentMetaState::Idle), "○");
    assert_eq!(agent_state_glyph(AgentMetaState::Done), "◆");
    assert_eq!(agent_state_glyph(AgentMetaState::Unknown), "○");
}

#[test]
fn no_record_falls_back_to_title_then_placeholder() {
    let workspace = ArrangementDoc::single(tid(1));
    // With a title: the OSC fallback (record outranks it when present).
    let mut meta = HashMap::new();
    meta.insert(
        tid(1),
        FleetPaneMeta {
            title: Some("vim src/main.rs".to_owned()),
            ..FleetPaneMeta::default()
        },
    );
    let items = local_items(&workspace, &HashMap::new(), &meta);
    assert_eq!(items[0].label, "○ 0.0 vim src/main.rs");
    assert_eq!(items[0].secondary, None, "no record => no state word");
    // Without a title: the placeholder.
    let items = local_items(&workspace, &HashMap::new(), &HashMap::new());
    assert_eq!(items[0].label, "○ 0.0 no agent");
}

#[test]
fn asked_flag_highlights_even_without_a_record() {
    let workspace = ArrangementDoc::single(tid(1));
    let mut meta = HashMap::new();
    meta.insert(
        tid(1),
        FleetPaneMeta {
            attention: true,
            ..FleetPaneMeta::default()
        },
    );
    let items = local_items(&workspace, &HashMap::new(), &meta);
    assert!(
        items[0].attention,
        "the ADR-0035 asked flag must highlight the row"
    );
    assert!(items[0].label.starts_with("● "), "{}", items[0].label);
}

#[test]
fn record_outranks_title_when_both_present() {
    let workspace = ArrangementDoc::single(tid(1));
    let mut agents = HashMap::new();
    agents.insert(tid(1), record("reviewer", None, AgentMetaState::Idle));
    let mut meta = HashMap::new();
    meta.insert(
        tid(1),
        FleetPaneMeta {
            title: Some("phux-ask: something".to_owned()),
            ..FleetPaneMeta::default()
        },
    );
    let items = local_items(&workspace, &agents, &meta);
    assert_eq!(
        items[0].label, "○ 0.0 reviewer",
        "ADR-0040 decision 3: the record must outrank the OSC title"
    );
}

#[test]
fn the_kind_is_shown_only_when_it_adds_to_the_name() {
    assert_eq!(who_label("reviewer", Some("claude")), "reviewer [claude]");
    assert_eq!(who_label("claude", Some("claude")), "claude");
    assert_eq!(who_label("Claude", Some("claude")), "Claude");
    assert_eq!(who_label("packer", None), "packer");
}

#[test]
fn secondary_prefers_branch_over_cwd_and_shortens_cwd() {
    let workspace = two_window_workspace();
    let mut agents = HashMap::new();
    agents.insert(tid(1), record("a", None, AgentMetaState::Working));
    agents.insert(tid(2), record("b", None, AgentMetaState::Idle));
    let mut meta = HashMap::new();
    meta.insert(
        tid(1),
        FleetPaneMeta {
            branch: Some("main".to_owned()),
            cwd: Some("/home/u/repo".to_owned()),
            ..FleetPaneMeta::default()
        },
    );
    meta.insert(
        tid(2),
        FleetPaneMeta {
            cwd: Some("/home/u/deep/dir/".to_owned()),
            ..FleetPaneMeta::default()
        },
    );
    let items = local_items(&workspace, &agents, &meta);
    assert_eq!(items[0].secondary.as_deref(), Some("working \u{b7} main"));
    assert_eq!(items[1].secondary.as_deref(), Some("idle \u{b7} dir"));
}

#[test]
fn sessions_order_current_first_then_by_name() {
    let workspace = ArrangementDoc::single(tid(1));
    let sessions = [
        sinfo(3, "zeta", 1),
        sinfo(1, "alpha", 1),
        sinfo(2, "work", 1),
    ];
    let items = fleet_items(
        &workspace,
        &sessions,
        Some(SessionId::new(2)),
        &HashMap::new(),
        &HashMap::new(),
        ForeignSessions {
            layouts: &HashMap::new(),
            agents: &HashMap::new(),
            terminals: &HashMap::new(),
        },
    );
    let headers: Vec<&str> = items
        .iter()
        .filter(|i| i.is_header())
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(headers, vec!["work (current)", "alpha", "zeta"]);
}

/// A foreign session with a cached layout lists one-step pane rows.
#[test]
fn foreign_session_with_cached_layout_lists_one_step_pane_rows() {
    let workspace = ArrangementDoc::single(tid(10));
    let sessions = [sinfo(1, "work", 1), sinfo(2, "scratch", 2)];
    // scratch's persisted layout: window 0 splits panes 20|21, window 1
    // is a single pane 22.
    let scratch = two_window_workspace_ids(20, 21, 22);
    let mut foreign_layouts = HashMap::new();
    foreign_layouts.insert(SessionId::new(2), scratch);
    // Agent records for two of scratch's panes.
    let mut foreign_agents = HashMap::new();
    foreign_agents.insert(
        tid(20),
        record("packer", Some("codex"), AgentMetaState::Working),
    );
    foreign_agents.insert(tid(21), record("linter", None, AgentMetaState::Blocked));

    let items = fleet_items(
        &workspace,
        &sessions,
        Some(SessionId::new(1)),
        &HashMap::new(),
        &HashMap::new(),
        ForeignSessions {
            layouts: &foreign_layouts,
            agents: &foreign_agents,
            terminals: &HashMap::new(),
        },
    );
    // work (current) header + its 1 pane, then scratch header + 3 pane
    // rows (no switch-session-only fallback).
    let scratch_hdr = items
        .iter()
        .position(|i| i.is_header() && i.label == "scratch")
        .expect("scratch header present");
    let rows = &items[scratch_hdr + 1..scratch_hdr + 4];
    assert!(
        rows.iter().all(|i| !i.is_header() && i.indented),
        "foreign session lists indented pane rows"
    );
    // Every foreign row commits a one-step switch-session carrying the
    // target session name, window, and pane.
    for r in rows {
        assert_eq!(r.action.action, "switch-session");
        assert_eq!(
            r.action.args.get("name"),
            Some(&toml::Value::String("scratch".to_owned()))
        );
        assert!(r.action.args.contains_key("window"));
        assert!(r.action.args.contains_key("pane"));
    }
    // First row addresses window 0 pane 0 and shows the codex agent.
    assert_eq!(rows[0].label, "◐ 0:main.0 packer [codex]");
    assert_eq!(rows[0].secondary.as_deref(), Some("working"));
    assert_eq!(
        rows[0].action.args.get("window"),
        Some(&toml::Value::Integer(0))
    );
    assert_eq!(
        rows[0].action.args.get("pane"),
        Some(&toml::Value::Integer(0))
    );
    // Blocked pane highlights (effective high attention).
    assert_eq!(rows[1].label, "● 0:main.1 linter");
    assert!(rows[1].attention, "blocked foreign pane must highlight");
    // Window 1 pane 0 has no record: `○` + placeholder, no state word.
    assert_eq!(rows[2].label, "○ 1:logs.0 no agent");
    assert_eq!(rows[2].secondary, None);
    assert_eq!(
        rows[2].action.args.get("window"),
        Some(&toml::Value::Integer(1))
    );
}

/// A foreign session with an EMPTY cached layout still falls
/// back to the single switch-session hop.
#[test]
fn foreign_session_with_empty_cached_layout_falls_back_to_switch_row() {
    let workspace = ArrangementDoc::single(tid(1));
    let sessions = [sinfo(1, "work", 1), sinfo(2, "scratch", 4)];
    let mut foreign_layouts = HashMap::new();
    foreign_layouts.insert(SessionId::new(2), ArrangementDoc::default());
    let items = fleet_items(
        &workspace,
        &sessions,
        Some(SessionId::new(1)),
        &HashMap::new(),
        &HashMap::new(),
        ForeignSessions {
            layouts: &foreign_layouts,
            agents: &HashMap::new(),
            terminals: &HashMap::from([(SessionId::new(2), 4)]),
        },
    );
    let scratch_hdr = items
        .iter()
        .position(|i| i.is_header() && i.label == "scratch")
        .expect("scratch header present");
    assert_eq!(items[scratch_hdr + 1].action.action, "switch-session");
    assert!(!items[scratch_hdr + 1].action.args.contains_key("window"));
    assert_eq!(
        items[scratch_hdr + 1].secondary.as_deref(),
        Some("4 terminals")
    );
}

#[test]
fn satellite_agents_group_by_name_with_a_host_badge() {
    let gpu = ResourceId::satellite("gpubox", 4);
    let edge = ResourceId::satellite("edge", 9);
    let local = ResourceId::local(1);
    let mut agents = HashMap::new();
    agents.insert(
        gpu,
        AgentRecord {
            name: "reviewer".to_owned(),
            state: AgentMetaState::Working,
            ..AgentRecord::default()
        },
    );
    agents.insert(
        edge.clone(),
        AgentRecord {
            name: "reviewer".to_owned(),
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        },
    );
    agents.insert(
        local,
        AgentRecord {
            name: "local-only".to_owned(),
            ..AgentRecord::default()
        },
    );
    let attention = HashSet::from([edge]);
    let items = satellite_agent_items(&agents, &attention, &ArrangementDoc::default());
    assert_eq!(items.len(), 3, "one header, two hosts: {items:?}");
    assert_eq!(items[0].label, "reviewer");
    assert!(items[0].is_header());
    assert_eq!(items[1].label, "● edge");
    assert!(items[1].attention, "the asked flag highlights the row");
    assert_eq!(items[1].action.action, "split-pane");
    assert_eq!(
        items[1]
            .action
            .args
            .get("resource")
            .and_then(|v| v.as_str()),
        Some("edge/@9")
    );
    assert_eq!(items[2].label, "◐ gpubox");
    assert!(!items[2].attention);
    assert_eq!(items[2].secondary.as_deref(), Some("working"));
}
