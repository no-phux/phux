//! The `phux ls --json` document (`SessionListJson`, `schema_version` 3),
//! built from one `GET_STATE` view.
//!
//! One builder for both surfaces: `phux ls --json` serializes it to stdout
//! and the MCP `phux_ls` tool returns it in-process, so the two cannot
//! drift (ADR-0022 §5).

use phux_core::session_list::{
    HostJson, HostSessionJson, ResourceExitJson, ResourceJson, SessionJson, SessionListJson,
};
use phux_protocol::SessionId;
use phux_protocol::wire::info::{
    ExitFacet, HostInventory, HostSessionInfo, SessionInfo, SessionSnapshot,
};

use crate::resource;
use crate::selector::format_terminal_id;
use crate::state::Degradation;

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
#[must_use]
pub fn document(
    snapshot: &SessionSnapshot,
    degradation: &Degradation,
    hosts_complete: bool,
) -> SessionListJson {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let entries = sessions.into_iter().map(session_json).collect();
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
/// are additive keys; `empty` is exactly `windows == 0`.
fn session_json(s: &SessionInfo) -> SessionJson {
    SessionJson {
        name: s.name.clone(),
        windows: s.window_count,
        attached: s.attached_client_count > 0,
        attached_clients: s.attached_client_count,
        keep_empty: s.keep_empty,
        empty: s.is_empty(),
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

/// Terminal-kind resources across one local session's windows.
fn local_pane_count(snapshot: &SessionSnapshot, session: SessionId) -> u16 {
    let panes = resource::terminals(snapshot)
        .filter(|pane| {
            snapshot
                .windows
                .iter()
                .any(|w| w.id == pane.window_id && w.session_id == session)
        })
        .count();
    u16::try_from(panes).unwrap_or(u16::MAX)
}

/// A local session's remembered focused pane as a canonical selector: the
/// active window's active resource, falling back to the session's first
/// window and then to that window's first Terminal.
fn local_active_terminal(snapshot: &SessionSnapshot, s: &SessionInfo) -> Option<String> {
    let window = s
        .active_window
        .and_then(|id| snapshot.windows.iter().find(|w| w.id == id))
        .or_else(|| snapshot.windows.iter().find(|w| w.session_id == s.id))?;
    let id = window.active_resource.clone().or_else(|| {
        resource::terminals(snapshot)
            .find(|pane| pane.window_id == window.id)
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

    fn session(name: &str, windows: u16, clients: u16) -> SessionInfo {
        SessionInfo::new(SessionId::new(1), name)
            .with_window_count(windows)
            .with_attached_client_count(clients)
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

    /// `--json` carries `keep_empty` and `empty` as added keys.
    #[test]
    fn json_rows_carry_keep_empty_and_empty() {
        let parked =
            serde_json::to_value(session_json(&session("parked", 0, 0).with_keep_empty(true)))
                .expect("serialize");
        assert_eq!(parked["keep_empty"], true);
        assert_eq!(parked["empty"], true);
        assert_eq!(parked["windows"], 0);

        let work = serde_json::to_value(session_json(&session("work", 2, 1))).expect("serialize");
        assert_eq!(work["keep_empty"], false);
        assert_eq!(work["empty"], false);
        assert_eq!(work["attached_clients"], 1);
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

        let doc = document(&snapshot, &Degradation::default(), true);
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
