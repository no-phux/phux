#[cfg(test)]
mod tests {
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
    use phux_client::layout::{Group, LayoutState};

    use super::*;

    fn snapshot(ids: &[u32]) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(
                ids.iter()
                    .map(|id| ResourceInfo::new(ResourceId::local(*id), WindowId::new(1), 80, 24))
                    .collect(),
            )
    }

    #[test]
    fn import_preserves_raw_recipe_and_resume_identity_without_resolving_templates() {
        let archive = model::parse_archive(r#"{"schema_version":2,"sessions":[{"name":"inert","windows":[{"name":"main","panes":[{"command":["old-disabled-provider","--dangerous"],"cwd":"/missing/archived/path","env":{"EXECUTABLE_INPUT":"exact"},"agent_session":{"plugin_id":"disabled-plugin","integration_id":"disabled-integration","native_id":"resume-123"}}]}]}]}"#).unwrap();
        let plan = model::restore_plan(&archive, &[]).unwrap();
        let create = &plan.creates[0];
        let launch = pane_launch(&PaneSource::seed(create), &create.name, &mut Vec::new()).unwrap();
        assert_eq!(launch.command, ["old-disabled-provider", "--dangerous"]);
        assert!(launch.env.iter().any(|(key, value)| key == "EXECUTABLE_INPUT" && value == "exact"));
        let provenance: serde_json::Value = serde_json::from_slice(launch.restore.as_ref().unwrap()).unwrap();
        assert_eq!(provenance["integration_id"], "disabled-integration");
        assert_eq!(provenance["native_id"], "resume-123");
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


    /// Against an `ORGANIZATION` server the archive reads and writes the
    /// typed arrangement, never the `phux.tui.layout/v1` key (ADR-0161 §9).
    mod organization {
        use phux_client::layout::wire::wire_leaves;
        use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
        use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
        use phux_protocol::wire::frame::{FrameKind, SessionLifetime};

        use super::*;

        fn member(key: ResourceKey, n: u8) -> ArrangementNode {
            ArrangementNode::Leaf(EntryRef::new([n; 16], EntryTarget::Member(key)))
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
            fixture.arrangement(work, WireDoc { groups });
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
            let workspace = ArrangementDoc::from_groups(vec![Group::new(
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
            )]);

            phux_client::org::replace_arrangement_of_member(&socket, &pane_a, &workspace)
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
}