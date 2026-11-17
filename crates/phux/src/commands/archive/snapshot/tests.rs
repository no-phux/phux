use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

use super::*;

/// Everything [`archive_from_snapshot`] reads besides the snapshot.
#[derive(Default)]
struct Inputs {
    attached: HashMap<ResourceId, AgentSessionRecord>,
    layouts: HashMap<SessionId, ArrangementDoc>,
    projects: HashMap<String, String>,
    hosts: HashMap<String, String>,
    directories: HashMap<String, String>,
}

fn export_with(snapshot: &SessionSnapshot, inputs: &Inputs) -> (WorkspaceArchive, Vec<String>) {
    archive_from_snapshot(
        snapshot,
        AgentRecords {
            attached: &inputs.attached,
            native: &HashMap::new(),
        },
        &inputs.layouts,
        &inputs.projects,
        &inputs.hosts,
        &inputs.directories,
    )
}

#[test]
fn projects_snapshot_into_workspace_archive() {
    let session = SessionInfo::new(SessionId::new(1), "ops")
        .with_active_window(Some(WindowId::new(2)))
        .with_window_count(1);
    let window = WindowInfo::new(WindowId::new(2), SessionId::new(1), "main")
        .with_active_resource(Some(ResourceId::local(3)));
    let pane = ResourceInfo::new(ResourceId::local(3), WindowId::new(2), 120, 40)
        .with_title(Some("monitor".to_owned()))
        .with_cwd(Some("/tmp/phux-ops".to_owned()));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(2), ResourceId::local(3))
        .with_sessions(vec![session])
        .with_windows(vec![window])
        .with_resources(vec![pane]);
    let agent_sessions = HashMap::from([(
        ResourceId::local(3),
        AgentSessionRecord::new("com.phux.agents", "claude-code", "session-42")
            .expect("valid record"),
    )]);

    let (archive, warnings) = export_with(
        &snapshot,
        &Inputs {
            attached: agent_sessions,
            ..Inputs::default()
        },
    );
    assert!(warnings.is_empty(), "{warnings:?}");

    assert_eq!(archive.schema_version, ARCHIVE_SCHEMA_VERSION);
    assert_eq!(archive.sessions[0].name, "ops");
    assert!(archive.sessions[0].active);
    assert_eq!(
        archive.sessions[0].windows[0].panes[0].title.as_deref(),
        Some("monitor")
    );
    assert_eq!(
        archive.sessions[0].windows[0].panes[0].cwd.as_deref(),
        Some("/tmp/phux-ops")
    );
    assert_eq!(archive.sessions[0].cwd.as_deref(), Some("/tmp/phux-ops"));
    assert_eq!(archive.sessions[0].host, None);
    assert_eq!(archive.sessions[0].windows[0].panes[0].command, None);
    assert_eq!(
        archive.sessions[0].windows[0].panes[0]
            .agent_session
            .as_ref()
            .map(|record| record.native_id.as_str()),
        Some("session-42")
    );
}

#[test]
fn marks_only_session_active_window_as_active() {
    let session = SessionInfo::new(SessionId::new(1), "ops")
        .with_active_window(Some(WindowId::new(3)))
        .with_window_count(2);
    let inactive_window = WindowInfo::new(WindowId::new(2), SessionId::new(1), "left")
        .with_active_resource(Some(ResourceId::local(4)));
    let active_window = WindowInfo::new(WindowId::new(3), SessionId::new(1), "right")
        .with_index(1)
        .with_active_resource(Some(ResourceId::local(5)));
    let panes = vec![
        ResourceInfo::new(ResourceId::local(4), WindowId::new(2), 80, 24),
        ResourceInfo::new(ResourceId::local(5), WindowId::new(3), 80, 24),
    ];
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(3), ResourceId::local(5))
        .with_sessions(vec![session])
        .with_windows(vec![inactive_window, active_window])
        .with_resources(panes);

    let (archive, warnings) = export_with(&snapshot, &Inputs::default());
    assert!(warnings.is_empty(), "{warnings:?}");

    assert!(!archive.sessions[0].windows[0].active);
    assert!(archive.sessions[0].windows[1].active);
}

#[test]
fn saves_the_focused_panes_host_directory_and_project() {
    let session = SessionInfo::new(SessionId::new(1), "api")
        .with_active_window(Some(WindowId::new(2)))
        .with_window_count(1);
    let pane_id = ResourceId::satellite("edge", 4);
    let window = WindowInfo::new(WindowId::new(2), SessionId::new(1), "main")
        .with_active_resource(Some(pane_id.clone()));
    let pane =
        ResourceInfo::new(pane_id, WindowId::new(2), 80, 24).with_cwd(Some("/src/api".to_owned()));
    let snapshot = SessionSnapshot::new(
        SessionId::new(1),
        WindowId::new(2),
        ResourceId::satellite("edge", 4),
    )
    .with_sessions(vec![session])
    .with_windows(vec![window])
    .with_resources(vec![pane]);
    let mut projects = HashMap::new();
    projects.insert("api".to_owned(), "phux".to_owned());

    let (archive, warnings) = export_with(
        &snapshot,
        &Inputs {
            projects,
            ..Inputs::default()
        },
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(archive.sessions[0].host.as_deref(), Some("edge"));
    assert_eq!(archive.sessions[0].cwd.as_deref(), Some("/src/api"));
    assert_eq!(archive.sessions[0].project.as_deref(), Some("phux"));
}

#[test]
fn a_stored_host_fills_a_local_pane() {
    let session = SessionInfo::new(SessionId::new(1), "api")
        .with_active_window(Some(WindowId::new(2)))
        .with_window_count(1);
    let pane_id = ResourceId::local(4);
    let window = WindowInfo::new(WindowId::new(2), SessionId::new(1), "main")
        .with_active_resource(Some(pane_id.clone()));
    let pane =
        ResourceInfo::new(pane_id, WindowId::new(2), 80, 24).with_cwd(Some("/src/api".to_owned()));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(2), ResourceId::local(4))
        .with_sessions(vec![session])
        .with_windows(vec![window])
        .with_resources(vec![pane]);
    let mut hosts = HashMap::new();
    hosts.insert("api".to_owned(), "edge".to_owned());
    let mut projects = HashMap::new();
    projects.insert("api".to_owned(), "phux".to_owned());

    let (archive, warnings) = export_with(
        &snapshot,
        &Inputs {
            projects,
            hosts,
            ..Inputs::default()
        },
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(archive.sessions[0].host.as_deref(), Some("edge"));
    assert_eq!(archive.sessions[0].cwd.as_deref(), Some("/src/api"));
    assert_eq!(archive.sessions[0].project.as_deref(), Some("phux"));
}

#[test]
fn a_stored_directory_wins_over_the_process_cwd() {
    let session = SessionInfo::new(SessionId::new(1), "api")
        .with_active_window(Some(WindowId::new(2)))
        .with_window_count(1);
    let pane_id = ResourceId::local(4);
    let window = WindowInfo::new(WindowId::new(2), SessionId::new(1), "main")
        .with_active_resource(Some(pane_id.clone()));
    let pane = ResourceInfo::new(pane_id, WindowId::new(2), 80, 24)
        .with_cwd(Some("/tmp/server".to_owned()));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(2), ResourceId::local(4))
        .with_sessions(vec![session])
        .with_windows(vec![window])
        .with_resources(vec![pane]);
    let mut directories = HashMap::new();
    directories.insert("api".to_owned(), "/src/api".to_owned());

    let (archive, warnings) = export_with(
        &snapshot,
        &Inputs {
            directories,
            ..Inputs::default()
        },
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(archive.sessions[0].cwd.as_deref(), Some("/src/api"));
}

/// Two sessions whose panes name them only through the `session` facet:
/// no legacy window, no `active_window`.
fn facet_only_snapshot() -> SessionSnapshot {
    let ops = SessionId::new(1);
    let api = SessionId::new(2);
    let pane = |id: u32, session: SessionId, cwd: &str| {
        ResourceInfo::new(ResourceId::local(id), WindowId::new(0), 80, 24)
            .with_session(Some(session))
            .with_cwd(Some(cwd.to_owned()))
    };
    SessionSnapshot::new(api, WindowId::new(0), ResourceId::local(5))
        .with_sessions(vec![
            SessionInfo::new(ops, "ops"),
            SessionInfo::new(api, "api"),
        ])
        .with_resources(vec![
            pane(3, ops, "/ops/a"),
            pane(4, ops, "/ops/b"),
            pane(5, api, "/api/b"),
            pane(6, api, "/api/a"),
        ])
}

fn export(
    snapshot: &SessionSnapshot,
    layouts: &HashMap<SessionId, ArrangementDoc>,
) -> (WorkspaceArchive, Vec<String>) {
    export_with(
        snapshot,
        &Inputs {
            layouts: layouts.clone(),
            ..Inputs::default()
        },
    )
}

fn pane_cwds(window: &WorkspaceWindow) -> Vec<&str> {
    window
        .panes
        .iter()
        .filter_map(|pane| pane.cwd.as_deref())
        .collect()
}

/// ADR-0161 §9: a snapshot with no windows still archives every pane in
/// its own session, focused on the snapshot's focused pane where known.
#[test]
fn exports_every_pane_by_session_facet_without_windows() {
    let (archive, warnings) = export(&facet_only_snapshot(), &HashMap::new());
    assert!(warnings.is_empty(), "{warnings:?}");

    let ops = &archive.sessions[0];
    assert_eq!(ops.name, "ops");
    assert!(!ops.active);
    assert_eq!(ops.windows.len(), 1);
    assert_eq!(ops.windows[0].name, SESSION_WINDOW_NAME);
    assert!(ops.windows[0].active);
    assert_eq!(pane_cwds(&ops.windows[0]), ["/ops/a", "/ops/b"]);
    // No focus on record: the first pane.
    assert!(ops.windows[0].panes[0].active);
    assert_eq!(ops.cwd.as_deref(), Some("/ops/a"));

    let api = &archive.sessions[1];
    assert!(api.active);
    assert_eq!(pane_cwds(&api.windows[0]), ["/api/b", "/api/a"]);
    assert!(
        api.windows[0].panes[0].active,
        "the snapshot's focused pane"
    );
    assert!(!api.windows[0].panes[1].active);
    assert_eq!(api.cwd.as_deref(), Some("/api/b"));
}

/// A stored layout reconciles against the facet-joined panes: none is
/// reported unplaced just because no window lists it.
#[test]
fn reconciles_a_layout_against_facet_joined_panes() {
    let tree = LayoutNode::Split {
        dir: SplitDir::Vertical,
        ratio: 0.5,
        left: Box::new(LayoutNode::Leaf(ResourceId::local(4))),
        right: Box::new(LayoutNode::Leaf(ResourceId::local(3))),
    };
    let layout = ArrangementDoc {
        groups: vec![Group::new(
            "main".to_owned(),
            LayoutState {
                focus: Some(ResourceId::local(3)),
                tree: Some(tree),
            },
        )],
        active: 0,
    };
    let layouts = HashMap::from([(SessionId::new(1), layout)]);

    let (archive, warnings) = export(&facet_only_snapshot(), &layouts);

    assert!(warnings.is_empty(), "{warnings:?}");
    let ops = &archive.sessions[0];
    assert_eq!(ops.windows.len(), 1);
    assert_eq!(ops.windows[0].name, "main");
    assert_eq!(pane_cwds(&ops.windows[0]), ["/ops/b", "/ops/a"]);
    assert!(ops.windows[0].panes[1].active);
    assert!(ops.windows[0].layout.is_some());
}

/// A leaf naming another session's pane is not this session's: it is
/// dropped, and this session's own panes are reported unplaced.
#[test]
fn a_layout_keeps_only_its_own_sessions_panes() {
    let layout = ArrangementDoc {
        groups: vec![Group::new(
            "main".to_owned(),
            LayoutState {
                focus: None,
                tree: Some(LayoutNode::Leaf(ResourceId::local(5))),
            },
        )],
        active: 0,
    };
    let layouts = HashMap::from([(SessionId::new(1), layout)]);

    let (archive, warnings) = export(&facet_only_snapshot(), &layouts);

    let ops = &archive.sessions[0];
    assert_eq!(ops.windows.len(), 1, "only the unplaced window");
    assert_eq!(ops.windows[0].name, "unplaced");
    assert_eq!(pane_cwds(&ops.windows[0]), ["/ops/a", "/ops/b"]);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

/// The session-level recent input target names the focused pane without
/// any legacy window.
#[test]
fn the_reported_recent_input_target_is_the_focused_pane() {
    let id = SessionId::new(1);
    let pane = |n, cwd: &str| {
        ResourceInfo::new(ResourceId::local(n), WindowId::new(0), 80, 24)
            .with_session(Some(id))
            .with_cwd(Some(cwd.to_owned()))
    };
    let snapshot = SessionSnapshot::new(id, WindowId::new(0), ResourceId::local(3))
        .with_sessions(vec![
            SessionInfo::new(id, "ops").with_recent_input(Some(ResourceId::local(4))),
        ])
        .with_resources(vec![pane(3, "/ops/a"), pane(4, "/ops/b")]);

    let (archive, _) = export_with(&snapshot, &Inputs::default());
    assert_eq!(archive.sessions[0].cwd.as_deref(), Some("/ops/b"));
}
