use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::info::{HostInventory, HostSessionInfo, ResourceInfo, WindowInfo};
use phux_protocol::{ResourceId, SessionId, WindowId};

use super::*;
use phux_protocol::wire::frame::SessionLifetime;

fn session(name: &str, windows: u16, clients: u16) -> SessionInfo {
    SessionInfo::new(SessionId::new(1), name)
        .with_window_count(windows)
        .with_attached_client_count(clients)
}

/// `phux ls` said "1 window" while the TUI showed two tabs: the server
/// holds one registry window per session, and the tabs live in the
/// layout envelope. A laid-out session reports its envelope's windows;
/// one never laid out keeps the server's count.
#[tokio::test]
async fn listing_counts_windows_from_the_layout_envelope() {
    use crate::layout::{ArrangementDoc, Group, LayoutState};
    use crate::layout_ops::{DEFAULT_LAYOUT_GROUP_ID, layout_key};
    use crate::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::wire::frame::Scope;

    let tabs = ArrangementDoc {
        groups: vec![
            Group::new("0".to_owned(), LayoutState::single(ResourceId::local(1))),
            Group::new("1".to_owned(), LayoutState::single(ResourceId::local(2))),
        ],
        active: 1,
    };
    let (tabbed, headless) = (SessionId::new(1), SessionId::new(2));
    let snapshot = SessionSnapshot::new(tabbed, WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(tabbed, "tabbed").with_window_count(1),
            SessionInfo::new(headless, "headless").with_window_count(1),
        ]);
    let spec = ScriptSpec::new().state(snapshot).stored_metadata(
        Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
        &layout_key(tabbed),
        tabs.encode_cbor().expect("encode"),
    );
    let (client, server) = tokio::net::UnixStream::pair().expect("pair");
    let server = tokio::spawn(ScriptedServer::on_stream(server, spec).run());
    let mut conn = Connection::from_stream(client);

    let view = listing_view(&mut conn).await.expect("listing");
    drop(conn);
    server.await.expect("scripted server task");
    let counts: Vec<(&str, u16)> = view
        .snapshot()
        .sessions
        .iter()
        .map(|s| (s.name.as_str(), s.window_count))
        .collect();
    assert_eq!(counts, [("tabbed", 2), ("headless", 1)]);
}

/// With organization records the tabs are the arrangement's groups, read
/// once per arranged session by key, with one inventory for the whole
/// listing; a session without an arrangement costs no read.
#[tokio::test]
async fn listing_counts_windows_from_each_arrangement_with_one_inventory() {
    use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, Command, EntryRef,
        EntryTarget, FrameKind,
    };

    use crate::testkit::{OrgFixture, ScriptSpec, serve_one};

    let mut fixture = OrgFixture::new();
    let tabbed = fixture.session("tabbed", SessionLifetime::Persistent);
    let (a, b) = (fixture.shell(tabbed), fixture.shell(tabbed));
    let group = |n: u8, key| ArrangementGroup {
        id: [n; 16],
        name: None,
        tree: ArrangementNode::Leaf(EntryRef {
            entry: [n; 16],
            target: EntryTarget::Member(key),
        }),
    };
    fixture.arrangement(
        tabbed,
        WireDoc {
            groups: vec![group(1, a), group(2, b)],
        },
    );
    let headless = fixture.session("headless", SessionLifetime::Persistent);
    fixture.shell(headless);
    let dir = tempfile::tempdir().expect("temp dir");
    let spec = ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .organization(fixture);
    let (socket, server) = serve_one(dir.path(), spec);
    let mut conn = Connection::connect(&socket).await.expect("connect");

    let view = listing_view(&mut conn).await.expect("listing");
    drop(conn);
    let seen = server.await.expect("scripted server task");
    let mut counts: Vec<(&str, u16)> = view
        .snapshot()
        .sessions
        .iter()
        .map(|s| (s.name.as_str(), s.window_count))
        .collect();
    counts.sort_unstable();
    assert_eq!(counts, [("headless", 1), ("tabbed", 2)]);
    let commands: Vec<&Command> = seen
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Command { command, .. } => Some(command),
            _ => None,
        })
        .collect();
    let inventories = commands
        .iter()
        .filter(|c| matches!(c, Command::GetOrganization { .. }))
        .count();
    let arrangements = commands
        .iter()
        .filter(|c| matches!(c, Command::ArrangementGet { .. }))
        .count();
    assert_eq!((inventories, arrangements), (1, 1), "{commands:?}");
}

/// A snapshot from a hub: one local session, a live satellite with one
/// session, and one the hub could not reach.
fn federated_snapshot() -> SessionSnapshot {
    let work = SessionId::new(1);
    SessionSnapshot::new(work, WindowId::new(10), ResourceId::local(7))
        .with_sessions(vec![
            SessionInfo::new(work, "work")
                .with_window_count(1)
                .with_active_window(Some(WindowId::new(10))),
        ])
        .with_windows(vec![
            WindowInfo::new(WindowId::new(10), work, "shell")
                .with_active_resource(Some(ResourceId::local(7))),
        ])
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(7),
            WindowId::new(10),
            80,
            24,
        )])
        .with_hosts(vec![
            HostInventory::reachable(
                SatelliteHost::new("edge"),
                vec![
                    HostSessionInfo::new(SessionId::new(1), "build")
                        .with_window_count(2)
                        .with_pane_count(3)
                        .with_attached_client_count(1)
                        .with_active_resource(Some(ResourceId::satellite(
                            SatelliteHost::new("edge"),
                            9,
                        ))),
                ],
            ),
            HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
        ])
}

fn empty_snapshot() -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
}

/// From a server without lifetimes, `keep_empty` and `empty` are what it
/// reported, and `lifetime` is `null`.
#[test]
fn json_rows_carry_keep_empty_and_empty_from_an_older_server() {
    let snapshot = empty_snapshot();
    let parked = serde_json::to_value(session_json(
        &snapshot,
        None,
        &session("parked", 0, 0).with_keep_empty(true),
    ))
    .expect("serialize");
    assert_eq!(parked["keep_empty"], true);
    assert_eq!(parked["empty"], true);
    assert_eq!(parked["windows"], 0);
    assert_eq!(parked["lifetime"], serde_json::Value::Null);

    let work = serde_json::to_value(session_json(&snapshot, None, &session("work", 2, 1)))
        .expect("serialize");
    assert_eq!(work["keep_empty"], false);
    assert_eq!(work["project"], serde_json::Value::Null);
    assert_eq!(work["empty"], false);
    assert_eq!(work["attached_clients"], 1);
}

/// ADR-0168: a session's project identity rides additively as `project`.
#[test]
fn json_rows_carry_the_project_identity() {
    let snapshot = empty_snapshot();
    let identity = phux_core::project_identity::SessionProject {
        id: "github.com/no-phux/phux".to_owned(),
        name: "phux".to_owned(),
        checkout: "/w/phux-feat".to_owned(),
        branch: Some("feat".to_owned()),
        linked_worktree: true,
        host: Some("mini".to_owned()),
    };
    let info = session("feat", 1, 0).with_attrs(identity.to_attrs());
    let row = serde_json::to_value(session_json(&snapshot, None, &info)).expect("serialize");
    assert_eq!(
        row["project"],
        serde_json::json!({
            "id": "github.com/no-phux/phux",
            "name": "phux",
            "checkout": "/w/phux-feat",
            "branch": "feat",
            "linked_worktree": true,
            "host": "mini",
        })
    );
}

/// Schema 4: lifetime, member count, and execution counts, from the
/// organization records when present and from the snapshot otherwise.
#[test]
fn json_rows_carry_lifetime_members_and_exec_counts() {
    use crate::testkit::OrgFixture;
    use phux_protocol::wire::frame::ExecState;

    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    fixture.resource(work, Some(ExecState::Running));
    fixture.resource(work, Some(ExecState::Completed));
    fixture.resource(work, Some(ExecState::Lost));
    fixture.resource(work, None);
    fixture.session("parked", SessionLifetime::Persistent);
    let snapshot = fixture.snapshot();
    let inventory = crate::org::OrgInventory {
        authority: phux_protocol::ids::AuthorityId::new([1; 16]).expect("non-zero"),
        inbox: None,
        sessions: fixture.sessions().to_vec(),
        resources: fixture.resources().to_vec(),
        snapshot_revision: phux_protocol::ids::Revision::new(1),
        journal_seq: 0,
        total_intake: 0,
        degradation: Degradation::default(),
    };
    let doc = document(&snapshot, Some(&inventory), &Degradation::default(), false);
    let json = serde_json::to_value(&doc).expect("serialize");
    assert_eq!(json["schema_version"], 4);
    let rows = json["sessions"].as_array().expect("sessions");
    let parked = &rows[0];
    assert_eq!(parked["name"], "parked");
    assert_eq!(parked["lifetime"], "persistent");
    assert_eq!(parked["members"], 0);
    assert_eq!(parked["empty"], true);
    assert_eq!(parked["keep_empty"], true);
    let work = &rows[1];
    assert_eq!(work["members"], 4);
    assert_eq!(work["exec"]["running"], 1);
    assert_eq!(work["exec"]["completed"], 1);
    assert_eq!(work["exec"]["lost"], 1);
    assert_eq!(work["exec"]["dormant"], 1);

    // Without the records: the one executing member the snapshot lists.
    let doc = document(&snapshot, None, &Degradation::default(), false);
    let json = serde_json::to_value(&doc).expect("serialize");
    let work = &json["sessions"][1];
    assert_eq!(work["lifetime"], "persistent");
    assert_eq!(work["members"], 1);
    assert_eq!(work["exec"]["running"], 1);
}

/// Session and resource rows carry the record's full key, the text a
/// workload `session:<key>` / `resource:<key>` scope names, never the
/// `^prefix` the listings print; `null` without organization records.
#[test]
fn json_rows_carry_full_record_keys() {
    use crate::testkit::OrgFixture;

    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let shell = fixture.shell(work);
    let live = fixture.live(&shell).expect("the shell executes");
    let snapshot = fixture.snapshot();
    let inventory = crate::org::OrgInventory {
        authority: phux_protocol::ids::AuthorityId::new([1; 16]).expect("non-zero"),
        inbox: None,
        sessions: fixture.sessions().to_vec(),
        resources: fixture.resources().to_vec(),
        snapshot_revision: phux_protocol::ids::Revision::new(1),
        journal_seq: 0,
        total_intake: 0,
        degradation: Degradation::default(),
    };
    let doc = document(&snapshot, Some(&inventory), &Degradation::default(), false);
    let session_key = doc.sessions[0].key.clone().expect("session key");
    assert_eq!(session_key, work.to_string());
    assert_eq!(session_key.len(), phux_protocol::ids::KEY_TEXT_LEN);
    assert_eq!(
        phux_protocol::ids::SessionKey::parse(&session_key),
        Some(work),
        "the key round-trips as the text a workload scope names"
    );
    let row = doc
        .resources
        .iter()
        .find(|row| row.id == format_terminal_id(&live))
        .expect("the shell's row");
    assert_eq!(row.key.as_deref(), Some(shell.to_string().as_str()));

    let doc = document(&snapshot, None, &Degradation::default(), false);
    assert_eq!(doc.sessions[0].key, None);
    assert!(doc.resources.iter().all(|row| row.key.is_none()));
}

/// `--json` gains `hosts` beside `sessions`; `sessions` still lists this
/// host's sessions only, so an existing consumer is unaffected.
#[test]
fn json_hosts_group_by_host_and_leave_sessions_alone() {
    let snapshot = federated_snapshot();
    let groups = host_groups(&snapshot, true);

    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].host, None);
    assert!(groups[0].local && groups[0].reachable);
    assert_eq!(groups[0].sessions[0].name, "work");
    assert_eq!(groups[0].sessions[0].panes, 1);
    assert_eq!(groups[0].sessions[0].active_terminal.as_deref(), Some("@7"));

    assert_eq!(groups[1].host.as_deref(), Some("edge"));
    assert!(!groups[1].local && groups[1].reachable);
    let build = &groups[1].sessions[0];
    assert_eq!((build.windows, build.panes), (2, 3));
    assert!(build.attached && build.attached_clients == 1);
    assert_eq!(build.active_terminal.as_deref(), Some("edge/@9"));

    assert_eq!(groups[2].host.as_deref(), Some("down"));
    assert!(!groups[2].reachable);
    assert!(groups[2].sessions.is_empty());
    assert_eq!(
        groups[2].unreachable.as_deref(),
        Some("link is down"),
        "the hub's diagnostic rides with the degraded host"
    );

    let doc = document(&snapshot, None, &Degradation::default(), true);
    let sessions: Vec<String> = serde_json::to_value(&doc).expect("serialize")["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .map(|s| s["name"].as_str().expect("name").to_owned())
        .collect();
    assert_eq!(
        sessions,
        vec!["work"],
        "satellite sessions never join the local session list"
    );
}

/// Without `HOST_SESSIONS` the JSON grouping carries this host alone:
/// the satellites are unknown, not absent.
#[test]
fn json_hosts_without_the_feature_are_local_only() {
    let groups = host_groups(&federated_snapshot(), false);
    assert_eq!(groups.len(), 1);
    assert!(groups[0].local);
    assert_eq!(groups[0].host, None);
}

/// A server that reports resource sessions synthesizes its window
/// count; the listing reads the count from the session's Terminals
/// instead. An older server's count is kept.
#[test]
fn window_counts_come_from_the_session_facet() {
    let (busy, idle) = (SessionId::new(1), SessionId::new(2));
    let facet = SessionSnapshot::new(busy, WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(busy, "busy").with_window_count(7),
            SessionInfo::new(idle, "idle").with_window_count(7),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(99), 80, 24)
                .with_session(Some(busy)),
        ]);
    let mut counted = facet.clone();
    count_session_windows(&mut counted);
    let counts: Vec<u16> = counted.sessions.iter().map(|s| s.window_count).collect();
    assert_eq!(counts, [1, 0]);

    let mut legacy = facet;
    legacy.resources[0].session = None;
    count_session_windows(&mut legacy);
    let counts: Vec<u16> = legacy.sessions.iter().map(|s| s.window_count).collect();
    assert_eq!(counts, [7, 7]);
}

/// The recency counter rides the row when the server reports it and is
/// omitted otherwise, so older consumers see the same shape.
#[test]
fn json_rows_carry_the_recency_counter() {
    let snapshot = empty_snapshot();
    let row =
        |s: SessionInfo| serde_json::to_value(session_json(&snapshot, None, &s)).expect("json");
    assert_eq!(row(session("a", 1, 0).with_touched(Some(7)))["touched"], 7);
    assert!(row(session("a", 1, 0)).get("touched").is_none());
}
