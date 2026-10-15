//! The `phux ls --json` document (`SessionListJson`, `schema_version` 4),
//! built from one `GET_STATE` view and, from a server that advertises
//! `ORGANIZATION`, the organization records read beside it.
//!
//! One builder for both surfaces: `phux ls --json` serializes it to stdout
//! and the MCP `phux_ls` tool returns it in-process, so the two cannot
//! drift (ADR-0022 §5).

use phux_core::activation::{ExecState as CoreExecState, SessionLifetime as CoreLifetime};
use phux_core::session_list::{
    ExecCounts, HostJson, HostSessionJson, ResourceExitJson, ResourceJson, SessionJson,
    SessionListJson,
};
use phux_protocol::SessionId;
use phux_protocol::wire::frame::{ExecState, OrgFilter, ResourceLifecycle, SessionLifetime};
use phux_protocol::wire::info::{
    ExitFacet, HostInventory, HostSessionInfo, SessionInfo, SessionSnapshot,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::wire::HandleMap;
use crate::layout_ops::{LayoutOps, LayoutOpsError};
use crate::org::{OrgError, OrgInventory};
use crate::resource;
use crate::selector::format_terminal_id;
use crate::state::{Degradation, StateView};

/// The `GET_STATE` view a session listing (`phux ls -l`, `phux status`, MCP
/// `phux_ls`) is built from.
///
/// It carries window counts as a TUI shows them ([`count_layout_windows`])
/// and, when the server advertises `ORGANIZATION`, the organization records
/// ([`StateView::organization`]).
///
/// # Errors
///
/// [`crate::state::get_state_on`]'s errors, or a transport failure while
/// reading layouts or records.
pub async fn listing_view(conn: &mut Connection) -> Result<StateView, AttachError> {
    let mut view = glance_view(conn).await?;
    count_layout_windows(conn, &mut view).await?;
    Ok(view)
}

/// [`listing_view`] without the window counts, which the glance never
/// prints and which cost a read per session: one `GET_STATE` and the
/// organization records, whatever the fleet's size.
///
/// # Errors
///
/// As [`listing_view`].
pub async fn glance_view(conn: &mut Connection) -> Result<StateView, AttachError> {
    let mut view = crate::state::get_state_on(conn).await?;
    if crate::org::supported(conn) {
        match crate::org::fetch(conn, OrgFilter::All, LAYOUT_READ_FIRST_REQUEST_ID - 0x100).await {
            Ok(inventory) => view = view.with_organization(Some(inventory)),
            Err(OrgError::Attach(err)) => return Err(err),
            Err(OrgError::OutcomeUnknown { .. }) => return Err(AttachError::Disconnected),
            // A refused read (no INVENTORY grant) keeps the snapshot-only
            // listing.
            Err(_) => {}
        }
    }
    Ok(view)
}

/// First request id [`count_layout_windows`] reads layouts with; clear of
/// the `GET_STATE` that fetched the snapshot.
const LAYOUT_READ_FIRST_REQUEST_ID: u32 = 0x4c00;

/// Count each local session's windows the way a TUI or Cockpit shows them.
///
/// Windows are consumer vocabulary (ADR-0017): the tabs are the groups of
/// the session's arrangement, while the server's own `window_count` stays at
/// one per non-empty session however many groups exist. Each session with an
/// arrangement takes its group count; a session never laid out (created
/// headless and not yet attached), or whose arrangement this connection may
/// not read, keeps the server's count. Use a dedicated, unsubscribed
/// connection.
///
/// With organization records in `view`, each arrangement is read by its key
/// from those records; a session without one costs no read.
///
/// # Errors
///
/// Transport failures only; every other layout read failure keeps the
/// server's count.
pub async fn count_layout_windows(
    conn: &mut Connection,
    view: &mut StateView,
) -> Result<(), AttachError> {
    if crate::org::supported(conn) {
        return count_arrangement_groups(conn, view).await;
    }
    let mut request_id = LAYOUT_READ_FIRST_REQUEST_ID;
    for session in &mut view.snapshot_mut().sessions {
        let read = LayoutOps::new(conn, session.id, request_id).read().await;
        request_id = request_id.wrapping_add(0x100);
        match read {
            Ok(workspace) => {
                session.window_count = u16::try_from(workspace.groups.len()).unwrap_or(u16::MAX);
            }
            Err(LayoutOpsError::Transport(err) | LayoutOpsError::Org(OrgError::Attach(err))) => {
                return Err(err);
            }
            Err(_) => {}
        }
    }
    Ok(())
}

/// [`count_layout_windows`] on a server that keeps organization records: the
/// group count of each session's arrangement, placed with that session's
/// members as [`LayoutOps::read`] places them. Without the records (a
/// refused read) every session keeps the server's count.
async fn count_arrangement_groups(
    conn: &mut Connection,
    view: &mut StateView,
) -> Result<(), AttachError> {
    let Some(organization) = view.organization() else {
        return Ok(());
    };
    let mut counts = Vec::new();
    let mut request_id = LAYOUT_READ_FIRST_REQUEST_ID;
    for (index, session) in view.snapshot().sessions.iter().enumerate() {
        let Some(record) = organization.session_by_live(session.id) else {
            continue;
        };
        let Some(key) = record.arrangement else {
            continue;
        };
        let read = crate::org::arrangement(conn, request_id, key).await;
        request_id = request_id.wrapping_add(1);
        match read {
            Ok(value) => {
                let handles = HandleMap::from_pairs(
                    organization
                        .members(&record.key)
                        .map(|member| (member.key, member.live.clone())),
                );
                let (document, _) = crate::layout::wire::from_protocol(&value.document, &handles);
                counts.push((index, document.groups.len()));
            }
            Err(OrgError::Attach(err)) => return Err(err),
            Err(_) => {}
        }
    }
    let sessions = &mut view.snapshot_mut().sessions;
    for (index, groups) in counts {
        sessions[index].window_count = u16::try_from(groups).unwrap_or(u16::MAX);
    }
    Ok(())
}

/// The stable `phux ls --json` document for one fetched view.
///
/// Sessions are name-sorted, so the document is stable across runs.
/// `degradation` becomes the `unreachable` list: always present, empty when
/// the listing is complete, so a consumer reads completeness positively
/// instead of inferring it from a missing key.
///
/// `hosts_complete` is whether the server advertised `HOST_SESSIONS`: only
/// then is the `hosts` grouping the whole fleet. Without it the document
/// carries this host's group alone and says so (`"hosts_complete": false`).
///
/// `organization` supplies each session's lifetime, member count, and
/// execution counts; without it they are derived from the snapshot.
#[must_use]
pub fn document(
    snapshot: &SessionSnapshot,
    organization: Option<&OrgInventory>,
    degradation: &Degradation,
    hosts_complete: bool,
) -> SessionListJson {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let entries = sessions
        .into_iter()
        .map(|s| session_json(snapshot, organization, s))
        .collect();
    // `terminals` stays the Terminal-kind inventory — the ids a Terminal-facet
    // verb accepts — so a consumer that iterates it and calls `snapshot` on
    // each keeps working; every resource, with its kind and parent, is the
    // additive `resources` array.
    let terminals = resource::terminals(snapshot)
        .map(|pane| format_terminal_id(&pane.id))
        .collect();
    let resources = snapshot
        .resources
        .iter()
        .map(|pane| ResourceJson {
            id: format_terminal_id(&pane.id),
            kind: resource::kind_name(pane.kind).to_owned(),
            parent: pane.parent.as_ref().map(format_terminal_id),
            lifecycle: resource::lifecycle_name(pane.lifecycle).to_owned(),
            exit: pane.exit.as_ref().map(resource_exit_json),
        })
        .collect();
    SessionListJson::new(entries)
        .with_terminals(terminals)
        .with_resources(resources)
        .with_hosts(host_groups(snapshot, hosts_complete))
        .with_hosts_complete(hosts_complete)
        .with_unreachable(degradation.notices().to_vec())
}

/// One session's [`SessionJson`] row. `keep_empty` and `empty` (ADR-0105)
/// stay for older consumers: `keep_empty` is `lifetime == persistent` and
/// `empty` is `members == 0` (`windows == 0` from a server without lifetime).
///
/// With the session's organization record, `lifetime`, `members`, and
/// `exec` come from it; otherwise from the snapshot: its reported lifetime,
/// the resources it lists in the session, and their lifecycles (a retained
/// exited process counts as completed). `project` is the identity its
/// attributes carry (ADR-0168): the record's, else the snapshot's.
#[must_use]
pub fn session_json(
    snapshot: &SessionSnapshot,
    organization: Option<&OrgInventory>,
    s: &SessionInfo,
) -> SessionJson {
    let record = organization.and_then(|org| {
        s.key
            .and_then(|key| org.session(&key))
            .or_else(|| org.session_by_live(s.id))
    });
    let mut exec = ExecCounts::default();
    let (lifetime, members) = if let (Some(record), Some(org)) = (record, organization) {
        for member in org.members(&record.key) {
            exec.add(member.activation.map(|a| core_exec_state(a.state)));
        }
        (Some(record.lifetime), record.members)
    } else {
        let mut members = 0_u32;
        for info in resource::in_session(snapshot, s.id) {
            members = members.saturating_add(1);
            exec.add(Some(match info.lifecycle {
                ResourceLifecycle::Exited => CoreExecState::Completed,
                _ => CoreExecState::Running,
            }));
        }
        (s.lifetime, members)
    };
    let keep_empty = lifetime.map_or(s.keep_empty, |l| l == SessionLifetime::Persistent);
    let empty = if lifetime.is_some() {
        members == 0
    } else {
        s.is_empty()
    };
    SessionJson {
        name: s.name.clone(),
        windows: s.window_count,
        attached: s.attached_client_count > 0,
        attached_clients: s.attached_client_count,
        keep_empty,
        empty,
        lifetime: lifetime.map(core_lifetime),
        members,
        exec,
        project: record
            .and_then(|record| {
                phux_core::project_identity::SessionProject::from_attrs(
                    record
                        .attrs
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str())),
                )
            })
            .or_else(|| crate::project_identity::of_session(s)),
    }
}

const fn core_lifetime(lifetime: SessionLifetime) -> CoreLifetime {
    match lifetime {
        SessionLifetime::Persistent => CoreLifetime::Persistent,
        SessionLifetime::Ephemeral => CoreLifetime::Ephemeral,
    }
}

const fn core_exec_state(state: ExecState) -> CoreExecState {
    match state {
        ExecState::Starting => CoreExecState::Starting,
        ExecState::Running => CoreExecState::Running,
        ExecState::Stopping => CoreExecState::Stopping,
        ExecState::Completed => CoreExecState::Completed,
        ExecState::Failed => CoreExecState::Failed,
        ExecState::Lost => CoreExecState::Lost,
        ExecState::Abandoned => CoreExecState::Abandoned,
    }
}

/// A retained resource's exit facet as `ls --json` spells it (ADR-0124).
fn resource_exit_json(exit: &ExitFacet) -> ResourceExitJson {
    ResourceExitJson {
        status: exit.exit_status,
        signal: exit.signal,
        reason: resource::close_reason_name(exit.reason).map(str::to_owned),
        exited_at_ms: exit.exited_at_ms,
        retained_until_ms: exit.retained_until_ms,
    }
}

/// The `hosts` grouping: this host first, then every satellite the hub
/// reported, reachable or not. Always emitted, so a consumer reads the
/// grouping positively instead of inferring it from a missing key.
///
/// Without `HOST_SESSIONS` (`complete` false) the server does not report
/// its satellites, so only this host's group is emitted; the document's
/// `hosts_complete: false` says the rest is unknown rather than absent.
fn host_groups(snapshot: &SessionSnapshot, complete: bool) -> Vec<HostJson> {
    let mut groups = vec![local_host_group(snapshot)];
    if complete {
        groups.extend(snapshot.hosts().iter().map(satellite_host_group));
    }
    groups
}

fn local_host_group(snapshot: &SessionSnapshot) -> HostJson {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    HostJson {
        host: None,
        local: true,
        reachable: true,
        unreachable: None,
        sessions: sessions
            .into_iter()
            .map(|s| local_host_session(snapshot, s))
            .collect(),
    }
}

fn local_host_session(snapshot: &SessionSnapshot, s: &SessionInfo) -> HostSessionJson {
    HostSessionJson {
        name: s.name.clone(),
        id: s.id.get(),
        windows: s.window_count,
        panes: local_pane_count(snapshot, s.id),
        attached: s.attached_client_count > 0,
        attached_clients: s.attached_client_count,
        active_terminal: local_active_terminal(snapshot, s),
    }
}

/// Terminal-kind resources in one local session.
fn local_pane_count(snapshot: &SessionSnapshot, session: SessionId) -> u16 {
    u16::try_from(resource::terminals_in(snapshot, session).count()).unwrap_or(u16::MAX)
}

/// A local session's most recent input target as a canonical selector: its
/// legacy window's active resource, falling back to its first Terminal.
fn local_active_terminal(snapshot: &SessionSnapshot, s: &SessionInfo) -> Option<String> {
    let recent = s
        .active_window
        .and_then(|id| snapshot.windows.iter().find(|w| w.id == id))
        .or_else(|| snapshot.windows.iter().find(|w| w.session_id == s.id))
        .and_then(|window| window.active_resource.clone());
    let id = recent.or_else(|| {
        resource::terminals_in(snapshot, s.id)
            .next()
            .map(|pane| pane.id.clone())
    })?;
    Some(format_terminal_id(&id))
}

fn satellite_host_group(host: &HostInventory) -> HostJson {
    let mut sessions: Vec<_> = host.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    HostJson {
        host: Some(host.host.to_string()),
        local: false,
        reachable: host.is_reachable(),
        unreachable: host.unreachable.clone(),
        sessions: sessions.into_iter().map(satellite_host_session).collect(),
    }
}

fn satellite_host_session(s: &HostSessionInfo) -> HostSessionJson {
    HostSessionJson {
        name: s.name.clone(),
        id: s.id.get(),
        windows: s.window_count,
        panes: s.pane_count,
        attached: s.attached_client_count > 0,
        attached_clients: s.attached_client_count,
        active_terminal: s.active_resource.as_ref().map(format_terminal_id),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
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
}
