//! `agent_entries` and `notify_rows`: the sidebar's agent rows and the
//! notifier's, and what focusing a pane marks seen.

use super::*;

/// An `AgentMetaIndex` holding `records` and nothing else — the shape
/// `agent_entries` reads.
fn meta_index(records: HashMap<ResourceId, AgentRecord>) -> AgentMetaIndex {
    AgentMetaIndex {
        records,
        ..AgentMetaIndex::default()
    }
}

/// Mixed lifecycle states and review changes never move navigation rows.
#[test]
fn agent_entries_keep_layout_order_across_review_changes() {
    let working = ResourceId::local(1);
    let done = ResourceId::local(2);
    let blocked = ResourceId::local(3);
    let mut workspace = ArrangementDoc::single(working.clone());
    workspace.add_group("w2".to_owned(), done.clone());
    workspace.add_group("w3".to_owned(), blocked.clone());

    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    for id in [&working, &done, &blocked] {
        panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
    }
    let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
    for (id, name, state) in [
        (&working, "w", AgentMetaState::Working),
        (&done, "d", AgentMetaState::Done),
        (&blocked, "b", AgentMetaState::Blocked),
    ] {
        records.insert(
            id.clone(),
            AgentRecord {
                name: name.to_owned(),
                state,
                ..AgentRecord::default()
            },
        );
    }

    // Status must not reorder the working, done, blocked layout.
    let entries = agent_entries(
        &workspace,
        &panes,
        &meta_index(records.clone()),
        &HashMap::new(),
        &ReviewIndex::new(),
    );
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["w", "d", "b"],
        "navigation follows layout, not urgency"
    );

    // Visiting changes the badge in place.
    panes.get_mut(&done).expect("slot").seen = true;
    let entries = agent_entries(
        &workspace,
        &panes,
        &meta_index(records),
        &HashMap::new(),
        &ReviewIndex::new(),
    );
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["w", "d", "b"], "review does not move the row");
}

/// Focusing a pane flips `seen` exactly once, and that flip makes
/// `refresh_window_chrome` report a real change; re-marking reports
/// `false`.
#[test]
fn focusing_an_unreviewed_done_pane_flips_seen_and_dirties_the_chrome() {
    let working = ResourceId::local(1);
    let done = ResourceId::local(2);
    let mut workspace = ArrangementDoc::single(working.clone());
    workspace.add_group("w2".to_owned(), done.clone());

    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    for id in [&working, &done] {
        panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
    }
    let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
    for (id, name, state) in [
        (&working, "w", AgentMetaState::Working),
        (&done, "d", AgentMetaState::Done),
    ] {
        records.insert(
            id.clone(),
            AgentRecord {
                name: name.to_owned(),
                state,
                ..AgentRecord::default()
            },
        );
    }
    let meta = meta_index(records);

    // Completion stays in its original row while unreviewed.
    let names: Vec<String> = agent_entries(
        &workspace,
        &panes,
        &meta,
        &HashMap::new(),
        &ReviewIndex::new(),
    )
    .into_iter()
    .map(|e| e.name)
    .collect();
    assert_eq!(names, vec!["w", "d"], "completion preserves row order");

    // Prime the painters against that (stale) view — this is the paint the
    // focus action itself produced, one iteration before the flip.
    let mut sidebar_painter = SidebarPainter::new(crate::render::Theme::default());
    let mut vcs = VcsIndex::default();
    let theme = crate::render::Theme::default();
    let mut refresh = |painter: &mut SidebarPainter, panes: &HashMap<ResourceId, PaneSlot>| {
        let mut chrome = ChromeCtx {
            viewport: (80, 24),
            sidebar: None,
            status_bar: None,
            sidebar_painter: Some(painter),
            session_name: "",
            theme: &theme,
            leaves: &crate::attach::surface_leaf::LeafBook::default(),
        };
        let scene = PaneScene {
            workspace: &workspace,
            panes,
            focused: Some(&done),
            zoomed: None,
        };
        refresh_window_chrome(
            &mut chrome,
            scene,
            None,
            &meta,
            &mut vcs,
            &HashMap::new(),
            no_peers(),
        )
        .0
    };
    refresh(&mut sidebar_painter, &panes);

    // The user is now looking at the finished pane.
    let mut review = ReviewIndex::new();
    assert!(
        mark_focused_seen(&mut panes, &mut review, Some(&done)),
        "the first mark after a focus change must report the flip"
    );

    assert!(
        refresh(&mut sidebar_painter, &panes),
        "the seen flip must dirty the chrome, or nothing repaints the strip"
    );
    let entries = agent_entries(&workspace, &panes, &meta, &HashMap::new(), &review);
    assert_eq!(
        entries
            .iter()
            .map(|e| (e.name.as_str(), e.seen))
            .collect::<Vec<_>>(),
        vec![("w", false), ("d", true)],
        "the focused pane's row stays put and is the only reviewed bit"
    );

    assert!(
        !mark_focused_seen(&mut panes, &mut review, Some(&done)),
        "re-marking an already-seen pane must not report a flip"
    );
    assert!(
        !refresh(&mut sidebar_painter, &panes),
        "an unchanged chrome must stay zero-cost"
    );
}

/// Last-change timestamps are not navigation-order inputs.
#[test]
fn agent_entries_ignore_change_timestamps_for_ordering() {
    let old = ResourceId::local(1);
    let fresh = ResourceId::local(2);
    let never = ResourceId::local(3);
    let mut workspace = ArrangementDoc::single(old.clone());
    workspace.add_group("w2".to_owned(), fresh.clone());
    workspace.add_group("w3".to_owned(), never.clone());

    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
    for (id, name) in [(&old, "old"), (&fresh, "fresh"), (&never, "never")] {
        panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        records.insert(
            id.clone(),
            AgentRecord {
                name: name.to_owned(),
                state: AgentMetaState::Blocked,
                ..AgentRecord::default()
            },
        );
    }

    let now = std::time::Instant::now();
    let mut index = meta_index(records);
    index.change_at.insert(
        old,
        now.checked_sub(std::time::Duration::from_secs(60))
            .expect("clock has an hour of headroom"),
    );
    index.change_at.insert(fresh, now);
    // `never` has no clock entry at all.

    let entries = agent_entries(
        &workspace,
        &panes,
        &index,
        &HashMap::new(),
        &ReviewIndex::new(),
    );
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["old", "fresh", "never"]);
}

/// A declared record names the agent row; the OSC title is not consulted.
#[test]
fn agent_entries_prefer_the_declared_record() {
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
    slot.terminal.vt_write(b"\x1b]2;codex resume\x07");
    panes.insert(id.clone(), slot);
    let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
    records.insert(
        id,
        AgentRecord {
            name: "merge-queue-w5".to_owned(),
            state: AgentMetaState::Working,
            ..AgentRecord::default()
        },
    );

    let entries = agent_entries(
        &workspace,
        &panes,
        &meta_index(records),
        &HashMap::new(),
        &ReviewIndex::new(),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].window, 0);
    assert_eq!(
        entries[0].window_name, "1",
        "stored window name, herdr's workspace column"
    );
    assert_eq!(entries[0].name, "merge-queue-w5");
    assert_eq!(entries[0].state, AgentMetaState::Working);
    assert!(!entries[0].attention);
}

/// Titles and attention flags alone never make a shell an agent row.
#[test]
fn agent_entries_require_structured_identity() {
    let claude = ResourceId::local(1);
    let shell = ResourceId::local(2);
    let mut workspace = ArrangementDoc::single(claude.clone());
    workspace.add_group("scratch".to_owned(), shell.clone());
    let (_, _, panes) = published_test_state(&[
        (&claude, 80, 24, b"\x1b]2;Claude Code - ~/src/phux\x07"),
        (&shell, 80, 24, b"\x1b]2;~/src/phux\x07"),
    ]);

    let entries = agent_entries(
        &workspace,
        &panes,
        &AgentMetaIndex::default(),
        &HashMap::new(),
        &ReviewIndex::new(),
    );
    assert!(
        entries.is_empty(),
        "title and attention state are not agent identity",
    );
}

/// A record declaring (or deriving) high attention marks
/// the entry even without the asked flag.
#[test]
fn agent_entries_carry_record_attention() {
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
    let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
    records.insert(
        id,
        AgentRecord {
            name: "reviewer".to_owned(),
            // Blocked derives high attention when none is declared.
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        },
    );

    let entries = agent_entries(
        &workspace,
        &panes,
        &meta_index(records),
        &HashMap::new(),
        &ReviewIndex::new(),
    );
    assert!(entries[0].attention);
}

/// The notifier keys a local row by its pane, names the attached session,
/// marks the focused pane as viewed, and still sees an ask on a pane with
/// no agent identity.
#[test]
fn bare_asks_reach_the_sidebar_and_the_notifier() {
    use crate::attach::notify::Phase;
    let agent = ResourceId::local(1);
    let shell = ResourceId::local(2);
    let mut workspace = ArrangementDoc::single(agent.clone());
    workspace
        .groups
        .push(ArrangementDoc::single(shell.clone()).groups.remove(0));
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    panes.insert(
        agent.clone(),
        PaneSlot::new_with_size(80, 24).expect("slot"),
    );
    let mut asking = PaneSlot::new_with_size(80, 24).expect("slot");
    asking.attention = true;
    asking.last_title = " deploy ".to_owned();
    panes.insert(shell.clone(), asking);
    let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
    records.insert(
        agent.clone(),
        AgentRecord {
            name: "reviewer".to_owned(),
            state: AgentMetaState::Working,
            ..AgentRecord::default()
        },
    );
    let entries = agent_entries(
        &workspace,
        &panes,
        &meta_index(records),
        &HashMap::new(),
        &ReviewIndex::new(),
    );
    // The sidebar lists the bare ask too, not "none running".
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[1].name, "deploy");
    assert!(entries[1].attention);
    let rows = notify_rows(&entries, &workspace, Some(&agent), "dev");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].key, agent);
    assert_eq!(rows[0].name, "reviewer");
    assert_eq!(rows[0].phase, Phase::Working);
    assert_eq!(rows[0].session, "dev");
    assert!(rows[0].viewing);
    assert_eq!(rows[1].key, shell);
    assert_eq!(rows[1].name, "deploy");
    assert_eq!(rows[1].phase, Phase::Blocked);
    assert!(rows[1].asking);
    assert!(!rows[1].viewing);
}

/// A bound `AgentSession` stream's state outranks the record, which still
/// names the row.
#[test]
fn agent_entries_take_state_from_the_stream_and_name_from_the_record() {
    use crate::attach::agent_rows::AgentSessionRow;
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
    let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
    records.insert(
        id.clone(),
        AgentRecord {
            name: "reviewer".to_owned(),
            state: AgentMetaState::Idle,
            ..AgentRecord::default()
        },
    );
    let mut sessions: AgentSessionRows = HashMap::new();
    sessions.insert(
        id,
        vec![AgentSessionRow {
            id: ResourceId::local(9),
            provider: Some("claude".to_owned()),
            native_id: Some("s-1".to_owned()),
            state: AgentMetaState::Blocked,
        }],
    );

    let entries = agent_entries(
        &workspace,
        &panes,
        &meta_index(records),
        &sessions,
        &ReviewIndex::new(),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "reviewer", "the record names the row");
    assert_eq!(
        entries[0].state,
        AgentMetaState::Blocked,
        "the stream's state outranks the record's"
    );
    assert!(entries[0].attention, "a blocked stream wants a human");
    assert_eq!(entries[0].pane, Some(0), "the parent pane is the target");
}

/// Without a record the stream still yields a row: the provider is the
/// name, and a pane whose OSC title says nothing agent-like still lists.
#[test]
fn agent_entries_list_a_stream_without_any_record() {
    use crate::attach::agent_rows::AgentSessionRow;
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
    let mut sessions: AgentSessionRows = HashMap::new();
    sessions.insert(
        id,
        vec![
            AgentSessionRow {
                id: ResourceId::local(8),
                provider: Some("codex".to_owned()),
                native_id: None,
                state: AgentMetaState::Working,
            },
            AgentSessionRow {
                id: ResourceId::local(9),
                provider: None,
                native_id: None,
                state: AgentMetaState::Done,
            },
        ],
    );

    let entries = agent_entries(
        &workspace,
        &panes,
        &AgentMetaIndex::default(),
        &sessions,
        &ReviewIndex::new(),
    );
    let names: Vec<(&str, AgentMetaState)> =
        entries.iter().map(|e| (e.name.as_str(), e.state)).collect();
    // Stream declaration order survives differing states.
    assert_eq!(
        names,
        vec![
            ("codex", AgentMetaState::Working),
            ("agent", AgentMetaState::Done)
        ]
    );
    assert!(!entries[1].attention);
}
