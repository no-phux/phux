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
    count_session_windows(view.snapshot_mut());
    Ok(view)
}

/// Replace each local session's server-reported window count with one read
/// from the session's own resources, when the server reports each
/// resource's session (ADR-0161 §7).
///
/// Such a server's windows are synthesized, one per non-empty session, so
/// the count is whether the session holds a Terminal; [`count_layout_windows`]
/// then replaces it with the arrangement's group count where it can read
/// one.
/// An older server's count is its own and stays.
fn count_session_windows(snapshot: &mut SessionSnapshot) {
    if !resource::reports_sessions(snapshot) {
        return;
    }
    let counts: Vec<u16> = snapshot
        .sessions
        .iter()
        .map(|session| {
            u16::from(
                resource::terminals_in(snapshot, session.id)
                    .next()
                    .is_some(),
            )
        })
        .collect();
    for (session, count) in snapshot.sessions.iter_mut().zip(counts) {
        session.window_count = count;
    }
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
/// the listing is complete.
///
/// `hosts_complete` is whether the server advertised `HOST_SESSIONS`: only
/// then is the `hosts` grouping the whole fleet. Without it the document
/// carries this host's group alone and says so (`"hosts_complete": false`).
///
/// `organization` supplies each session's lifetime, member count, and
/// execution counts (else derived from the snapshot), and every record's
/// full `key`, which a `session:` or `resource:` scope names.
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
    // `terminals` stays the Terminal-kind inventory (the ids a Terminal-facet
    // verb accepts); every resource is the additive `resources` array.
    let terminals = resource::terminals(snapshot)
        .map(|pane| format_terminal_id(&pane.id))
        .collect();
    let resources = snapshot
        .resources
        .iter()
        .map(|pane| ResourceJson {
            id: format_terminal_id(&pane.id),
            kind: resource::kind_name(pane.kind).to_owned(),
            name: pane.title.clone(),
            parent: pane.parent.as_ref().map(format_terminal_id),
            lifecycle: resource::lifecycle_name(pane.lifecycle).to_owned(),
            exit: pane.exit.as_ref().map(resource_exit_json),
            producer: resource::producer_name(pane).map(str::to_owned),
            control: pane.control.map(|class| class.name().to_owned()),
            key: organization
                .and_then(|org| org.resource_by_live(&pane.id))
                .map(|record| record.key.to_string()),
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
/// attributes carry (ADR-0168): the record's, else the snapshot's. `key` is
/// the record's full key text, `None` without a record.
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
        project_link: record
            .map_or_else(
                || crate::project_identity::link_of_session(s),
                |record| crate::project_identity::link_of_attrs(&record.attrs),
            )
            .map(str::to_owned),
        key: record.map(|record| record.key.to_string()),
        touched: s.touched,
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

/// A local session's most recent input target as a canonical selector
/// ([`resource::recent_terminal`]), falling back to its first Terminal.
fn local_active_terminal(snapshot: &SessionSnapshot, s: &SessionInfo) -> Option<String> {
    let id = resource::recent_terminal(snapshot, s).or_else(|| {
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
mod tests;
