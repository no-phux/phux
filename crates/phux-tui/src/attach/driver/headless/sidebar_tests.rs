use super::*;

#[test]
fn headless_blocked_agent_has_matching_session_summary() {
    use phux_client::agent_meta::{AgentMetaState, AgentRecord};
    let kernel = SessionKernel::new(
        GhosttyAdapter::new(phux_protocol::BootstrapLimits::default()),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let chrome = HeadlessChrome {
        sidebar: Some(SidebarReservation {
            edge: crate::attach::paint::SidebarEdge::Left,
            width: 32,
        }),
        theme: crate::render::Theme::default(),
        agents_share: crate::render::chrome::sidebar::DEFAULT_AGENTS_SHARE,
        status_bar: None,
    };
    let mut session = HeadlessSession::new(kernel, chrome, (100, 24));
    let id = ResourceId::local(1);
    session.mirror.workspace = crate::layout::ArrangementDoc::single(id.clone());
    session.mirror.session_name = "work".to_owned();
    session.mirror.agent_meta.records.insert(
        id,
        AgentRecord {
            name: "reviewer".to_owned(),
            state: AgentMetaState::Blocked,
            ..Default::default()
        },
    );
    let frame = session.compose();
    let rows: Vec<String> = frame
        .cells
        .chunks(100)
        .map(|row| {
            row[..32]
                .iter()
                .map(|cell| cell.grapheme.as_str())
                .collect()
        })
        .collect();
    // The blocked agent badges its tab, the session row counts it, and
    // the Agents panel lists it: the panel shows every agent.
    let work_row = rows
        .iter()
        .position(|row| row.contains("work"))
        .expect("session row");
    assert!(rows[work_row].contains("●1"), "{rows:?}");
    // The tab row, nested under its session, carries the agent's badge.
    assert!(rows[work_row + 1].contains("● reviewer"), "{rows:?}");
    assert_eq!(
        rows.iter().filter(|row| row.contains("reviewer")).count(),
        2,
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("Agents")), "{rows:?}");
}

/// ADR-0161: against an `ORGANIZATION` server the composite's layout is
/// the session's arrangement record, never the translated layout key.
#[tokio::test(flavor = "current_thread")]
async fn headless_layout_is_the_arrangement_record() {
    use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, EntryRef, EntryTarget,
        SessionLifetime,
    };
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let shell = fixture.shell(work);
    fixture.arrangement(
        work,
        WireDoc {
            groups: vec![ArrangementGroup {
                id: [7; 16],
                name: Some("build".to_owned()),
                tree: ArrangementNode::Leaf(EntryRef {
                    entry: [1; 16],
                    target: EntryTarget::Member(shell),
                }),
            }],
        },
    );
    let session_id = fixture.live_session(&work).expect("live session");
    let pane = fixture.live(&shell).expect("live pane");
    let mut snapshot = fixture.snapshot();
    snapshot.focused_session = session_id;
    snapshot.focused_resource = pane.clone();
    let dir = tempfile::tempdir().expect("dir");
    let spec = ScriptSpec::new()
        .server_features_ext(phux_protocol::ServerFeatureExtSet::with(&[
            phux_protocol::ServerFeatureExt::Organization,
        ]))
        .organization(fixture);
    let (socket, _server) = serve_one(dir.path(), spec);

    let kernel = SessionKernel::new(
        GhosttyAdapter::new(phux_protocol::BootstrapLimits::default()),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let chrome = HeadlessChrome {
        sidebar: None,
        theme: crate::render::Theme::default(),
        agents_share: crate::render::chrome::sidebar::DEFAULT_AGENTS_SHARE,
        status_bar: None,
    };
    let mut session = HeadlessSession::new(kernel, chrome, (80, 24));
    session
        .ingest(
            FrameKind::Attached {
                attach_id: 1,
                initial_client_id: phux_protocol::ids::ClientId::new(1),
                snapshot,
            },
            None,
            None,
        )
        .expect("attached");
    let document = read_arrangement(&socket, session_id)
        .await
        .expect("arrangement read");
    session
        .ingest_layout(document.as_ref(), session_id)
        .expect("ingest");
    let groups = &session.mirror.workspace.groups;
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "build");
    assert_eq!(
        crate::layout::leaves(groups[0].state.tree.as_ref().expect("tree")),
        vec![pane]
    );
}

/// The composite's dividers paint with the configured theme, as the
/// glass's do, not the built-in default.
#[test]
fn headless_dividers_use_the_configured_theme() {
    use phux_core::screen::CellColor;
    use ratatui::style::Color;
    let kernel = SessionKernel::new(
        GhosttyAdapter::new(phux_protocol::BootstrapLimits::default()),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let theme = crate::render::Theme {
        divider: Color::Rgb(1, 2, 3),
        divider_focus: Color::Rgb(4, 5, 6),
        ..crate::render::Theme::default()
    };
    let chrome = HeadlessChrome {
        sidebar: None,
        theme,
        agents_share: crate::render::chrome::sidebar::DEFAULT_AGENTS_SHARE,
        status_bar: None,
    };
    let mut session = HeadlessSession::new(kernel, chrome, (80, 24));
    let (left, right) = (ResourceId::local(1), ResourceId::local(2));
    let mut workspace = crate::layout::ArrangementDoc::single(left.clone());
    let tree = workspace
        .active_group()
        .and_then(|window| window.tree.clone())
        .expect("tree");
    workspace.active_group_mut().expect("window").tree = Some(
        crate::layout::split_at(
            &tree,
            &left,
            &right,
            crate::layout::SplitDir::Horizontal,
            0.5,
        )
        .expect("split"),
    );
    session.mirror.workspace = workspace;
    let frame = session.compose();
    let themed = |cell: &phux_core::screen::RenderedCell| {
        matches!(
            cell.style.fg,
            CellColor::Rgb { r: 1, g: 2, b: 3 } | CellColor::Rgb { r: 4, g: 5, b: 6 }
        )
    };
    assert!(
        frame.cells.iter().any(themed),
        "no divider cell carries the configured theme"
    );
}
