use phux_protocol::ids::{SessionId, WindowId};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::*;

fn snapshot(ids: &[u32]) -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1)).with_resources(
        ids.iter()
            .map(|id| ResourceInfo::new(ResourceId::local(*id), WindowId::new(1), 80, 24))
            .collect(),
    )
}

#[test]
fn a_restored_session_carries_its_project_link_beside_its_label() {
    let archive = model::parse_archive(
        r#"{"schema_version": 2, "sessions": [
            {"name": "api", "project": "api", "project_link": "phux", "windows": []}
        ]}"#,
    )
    .expect("archive parses");
    let plan = model::restore_plan(&archive, &[]).expect("restore plan");
    let attrs = session_attrs(&plan.creates[0], ImportKind::Archive);
    assert!(attrs.contains(&("project".to_owned(), "api".to_owned())));
    assert!(attrs.contains(&(
        phux_core::project_identity::attr::LINK.to_owned(),
        "phux".to_owned()
    )));
}

#[test]
fn save_guard_rejects_reaped_or_new_local_terminals() {
    assert!(same_local_terminals(&snapshot(&[1, 2]), &snapshot(&[2, 1])));
    assert!(!same_local_terminals(&snapshot(&[1, 2]), &snapshot(&[1])));
    assert!(!same_local_terminals(
        &snapshot(&[1, 2]),
        &snapshot(&[1, 2, 3])
    ));
}

fn pane_grid(rows: &[&[u32]]) -> Vec<Vec<Option<ResourceId>>> {
    rows.iter()
        .map(|row| row.iter().map(|id| Some(ResourceId::local(*id))).collect())
        .collect()
}

fn window(
    name: &str,
    active: bool,
    panes: usize,
    layout: Option<model::WorkspaceLayoutNode>,
) -> WorkspaceWindow {
    WorkspaceWindow {
        name: name.to_owned(),
        active,
        layout,
        panes: (0..panes)
            .map(|i| model::WorkspacePane {
                active: i == 0,
                title: None,
                cwd: None,
                command: None,
                agent_session: None,
                native_launch: None,
                env: std::collections::BTreeMap::new(),
                cols: 0,
                rows: 0,
            })
            .collect(),
    }
}

/// ADR-0129: replaying the archived split tree produces one window per
/// archived window, each with a fresh identity, and a leaf per archived
/// pane at the position the archive named — not just one preferred pane.
#[test]
fn build_restored_workspace_replays_the_split_tree_with_fresh_window_ids() {
    let split = model::WorkspaceLayoutNode::Split {
        dir: model::WorkspaceSplitDir::Vertical,
        ratio: 0.6,
        left: Box::new(model::WorkspaceLayoutNode::Pane { pane: 0 }),
        right: Box::new(model::WorkspaceLayoutNode::Pane { pane: 1 }),
    };
    let windows = vec![
        window("main", true, 2, Some(split)),
        window("logs", false, 1, None),
    ];
    let pane_ids = pane_grid(&[&[10, 11], &[12]]);

    let workspace = build_restored_workspace(&windows, &pane_ids).expect("builds");
    assert_eq!(workspace.groups.len(), 2);
    assert_eq!(
        workspace.active, 0,
        "the archived active window stays active"
    );

    let main = &workspace.groups[0];
    assert_eq!(main.name, "main");
    assert!(matches!(
        main.state.tree,
        Some(LayoutNode::Split {
            dir: SplitDir::Vertical,
            ..
        })
    ));
    assert_eq!(
        phux_client::layout::leaves(main.state.tree.as_ref().unwrap()),
        vec![ResourceId::local(10), ResourceId::local(11)]
    );

    let logs = &workspace.groups[1];
    assert_eq!(logs.name, "logs");
    assert_eq!(
        phux_client::layout::leaves(logs.state.tree.as_ref().unwrap()),
        vec![ResourceId::local(12)],
        "a window with no captured layout still places every archived pane"
    );

    assert_ne!(
        main.id, logs.id,
        "each restored window gets its own fresh identity"
    );
    assert_ne!(
        main.id, [0; 16],
        "a fresh window identity is never the zero sentinel"
    );
}

#[test]
fn linear_chain_places_every_pane_when_no_layout_was_captured() {
    let ids = vec![
        ResourceId::local(1),
        ResourceId::local(2),
        ResourceId::local(3),
    ];
    let tree = linear_chain(&ids);
    assert_eq!(phux_client::layout::leaves(&tree), ids);
}

#[test]
fn an_unroutable_host_keeps_the_local_session() {
    let err = "archive relaunch could not recreate an archived pane: spawn failed: no route to that satellite";
    assert!(keep_local_when_host_unroutable(Some("edge"), err));
    assert!(keep_local_when_host_unroutable(
        Some("edge"),
        "session was saved on host \"edge\" but the archive has no pane to restore there"
    ));
    assert!(!keep_local_when_host_unroutable(None, err));
    assert!(!keep_local_when_host_unroutable(Some(""), err));
    assert!(!keep_local_when_host_unroutable(
        Some("edge"),
        "archive relaunch could not recreate an archived pane: spawn failed: permission denied"
    ));
}

#[test]
fn layout_node_from_archive_rejects_an_out_of_range_pane_index() {
    let node = model::WorkspaceLayoutNode::Pane { pane: 5 };
    let ids = [ResourceId::local(1)];
    assert!(layout_node_from_archive(&node, &ids).is_none());
}

/// Against an `ORGANIZATION` server the archive reads and writes the
/// typed arrangement, never the `phux.tui.layout/v1` key (ADR-0161 §9).
mod organization {
    use phux_client::layout::wire::wire_leaves;
    use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
    use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
    use phux_protocol::wire::frame::{FrameKind, SessionLifetime};

    use super::*;

    fn member(key: ResourceKey, n: u8) -> ArrangementNode {
        ArrangementNode::Leaf(EntryRef {
            entry: [n; 16],
            target: EntryTarget::Member(key),
        })
    }

    /// A session with two shells, `a` arranged alone in group "main".
    fn arranged() -> (OrgFixture, SessionKey, ResourceKey, ResourceKey) {
        arranged_as(Some("main"), true)
    }

    /// A session with two shells and a replaced arrangement: `a` alone
    /// in one group named `name`, or no group at all.
    fn arranged_as(
        name: Option<&str>,
        placed: bool,
    ) -> (OrgFixture, SessionKey, ResourceKey, ResourceKey) {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        let a = fixture.shell(work);
        let b = fixture.shell(work);
        let groups = placed
            .then(|| ArrangementGroup::new([7; 16], name.map(str::to_owned), member(a, 1)))
            .into_iter()
            .collect();
        fixture.arrangement(
            work,
            WireDoc {
                groups,
                stacks: false,
            },
        );
        (fixture, work, a, b)
    }

    /// The organization records `fixture` serves, read as an export does.
    async fn records_of(fixture: OrgFixture) -> phux_client::org::OrgInventory {
        let (socket, server, _dir) = serve(fixture);
        let records = phux_client::org::fetch_from_socket(&socket)
            .await
            .expect("records");
        server.await.expect("server");
        records
    }

    /// Export's layout read against `fixture`, with every frame sent.
    async fn export_layouts(
        fixture: OrgFixture,
        projection: Option<&str>,
    ) -> (SessionLayouts, Vec<String>, Vec<FrameKind>) {
        let snapshot = fixture.snapshot();
        let records = records_of(fixture.clone()).await;
        let (socket, server, _dir) = serve(fixture);
        let (layouts, warnings) =
            fetch_session_layouts(&socket, &snapshot, projection, &records).await;
        (layouts, warnings, server.await.expect("server"))
    }

    fn serve(
        fixture: OrgFixture,
    ) -> (
        PathBuf,
        tokio::task::JoinHandle<Vec<FrameKind>>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = ScriptSpec::new()
            .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
            .organization(fixture);
        let (socket, server) = serve_one(dir.path(), spec);
        (socket, server, dir)
    }

    fn commands(seen: &[FrameKind]) -> Vec<&Command> {
        seen.iter()
            .filter_map(|frame| match frame {
                FrameKind::Command { command, .. } => Some(command),
                _ => None,
            })
            .collect()
    }

    fn touches_layout_metadata(seen: &[FrameKind]) -> bool {
        seen.iter().any(|frame| {
            matches!(
                frame,
                FrameKind::GetMetadata { .. } | FrameKind::SetMetadata { .. }
            )
        })
    }

    #[tokio::test]
    async fn export_reads_each_sessions_arrangement() {
        let (fixture, work, a, _b) = arranged();
        let session = fixture.live_session(&work).expect("live session");
        let pane_a = fixture.live(&a).expect("live pane");

        let (layouts, warnings, seen) = export_layouts(fixture, None).await;

        assert!(warnings.is_empty(), "{warnings:?}");
        let layout = layouts.get(&session).expect("the session's arrangement");
        assert_eq!(layout.groups.len(), 1);
        assert_eq!(layout.groups[0].name, "main");
        assert_eq!(
            phux_client::layout::leaves(layout.groups[0].state.tree.as_ref().expect("tree")),
            vec![pane_a]
        );
        assert!(
            commands(&seen)
                .iter()
                .any(|command| matches!(command, Command::ArrangementGet { .. })),
            "{seen:?}"
        );
        assert!(!touches_layout_metadata(&seen), "{seen:?}");
    }

    /// A named projection is its own metadata key even on an
    /// `ORGANIZATION` server; it never reads the arrangement.
    #[tokio::test]
    async fn export_under_a_projection_reads_its_key() {
        let (fixture, ..) = arranged();

        let (layouts, warnings, seen) = export_layouts(fixture, Some("myapp.layout/v1")).await;

        assert!(layouts.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            seen.iter()
                .any(|frame| matches!(frame, FrameKind::GetMetadata { key, .. } if key.starts_with("myapp.layout/v1/"))),
            "{seen:?}"
        );
        assert!(commands(&seen).is_empty(), "{seen:?}");
    }

    /// An unnamed group stays unnamed in the archive rather than taking
    /// its position as a name an import would then keep.
    #[tokio::test]
    async fn export_keeps_an_unnamed_group_unnamed() {
        let (fixture, work, ..) = arranged_as(None, true);
        let session = fixture.live_session(&work).expect("live session");

        let (layouts, ..) = export_layouts(fixture, None).await;

        let layout = layouts.get(&session).expect("the session's arrangement");
        assert_eq!(layout.groups[0].name, "");
    }

    /// Every session has an arrangement from creation; one nobody has
    /// arranged exports as the one-window projection, with no warning,
    /// as it did before it had a layout key.
    #[tokio::test]
    async fn an_empty_arrangement_exports_as_one_window() {
        let (fixture, ..) = arranged_as(None, false);
        let snapshot = fixture.snapshot().with_windows(Vec::new());

        let (layouts, layout_warnings, _seen) = export_layouts(fixture, None).await;
        let (archive, warnings) = archive_from_snapshot(
            &snapshot,
            snapshot::AgentRecords {
                attached: &HashMap::new(),
                native: &HashMap::new(),
            },
            &layouts,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );

        assert!(layouts.is_empty());
        assert!(layout_warnings.is_empty(), "{layout_warnings:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        let windows = &archive.sessions[0].windows;
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].name, "window-0");
        assert_eq!(windows[0].panes.len(), 2);
    }

    #[tokio::test]
    async fn restore_replaces_the_seed_sessions_arrangement() {
        let (fixture, _work, a, b) = arranged();
        let (pane_a, pane_b) = (
            fixture.live(&a).expect("live pane"),
            fixture.live(&b).expect("live pane"),
        );
        let (socket, server, _dir) = serve(fixture);
        let workspace = ArrangementDoc {
            groups: vec![Group::new(
                "restored".to_owned(),
                LayoutState {
                    tree: Some(LayoutNode::Split {
                        dir: SplitDir::Vertical,
                        ratio: 0.5,
                        left: Box::new(LayoutNode::Leaf(pane_a.clone())),
                        right: Box::new(LayoutNode::Leaf(pane_b)),
                    }),
                    focus: Some(pane_a.clone()),
                },
            )],
            active: 0,
        };

        write_restored_layout(&socket, &pane_a, workspace)
            .await
            .expect("the arrangement is replaced");

        let seen = server.await.expect("server");
        let replaced: Vec<&WireDoc> = commands(&seen)
            .into_iter()
            .filter_map(|command| match command {
                Command::ArrangementReplace { document, .. } => Some(document),
                _ => None,
            })
            .collect();
        assert_eq!(replaced.len(), 1, "{seen:?}");
        assert_eq!(replaced[0].groups[0].name.as_deref(), Some("restored"));
        let targets: Vec<EntryTarget> = wire_leaves(&replaced[0].groups[0].tree)
            .into_iter()
            .map(|entry| entry.target)
            .collect();
        assert_eq!(
            targets,
            vec![EntryTarget::Member(a), EntryTarget::Member(b)]
        );
        assert!(!touches_layout_metadata(&seen), "{seen:?}");
    }
}
