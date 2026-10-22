//! Unit tests for the host-grouped and flat session listings.

use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use phux_protocol::{ResourceId, SessionId, WindowId};

use super::lines::{format_row, format_session_line, with_key};
use super::{EMPTY_STATE, session_lines};

/// `phux ls --all` with no local server said "unreachable: attach loop
/// io error: No such file or directory (os error 2)".
#[test]
fn a_host_with_no_server_is_named_so() {
    use phux_client::attach::AttachError;
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::ConnectionRefused,
    ] {
        let err = AttachError::Io(std::io::Error::from(kind));
        assert_eq!(
            super::all_hosts::unreachable_reason(&err),
            "no server running"
        );
    }
    assert_eq!(
        super::all_hosts::unreachable_reason(&AttachError::Refused("no".to_owned())),
        AttachError::Refused("no".to_owned()).to_string()
    );
}

#[test]
fn all_hosts_listing_groups_by_machine_and_keeps_a_down_host() {
    use phux_core::activation::SessionLifetime;
    use phux_core::host_list::{HostJson, HostKind, HostListJson};
    use phux_core::session_list::{ExecCounts, SessionJson};
    let session = |name: &str, clients| SessionJson {
        name: name.to_owned(),
        windows: 2,
        attached: clients > 0,
        attached_clients: clients,
        keep_empty: true,
        empty: false,
        lifetime: Some(SessionLifetime::Persistent),
        members: 2,
        exec: ExecCounts {
            running: 2,
            ..ExecCounts::default()
        },
        project: None,
        project_link: None,
        key: None,
    };
    let doc = HostListJson::new(vec![
        HostJson {
            name: "local".to_owned(),
            label: "laptop".to_owned(),
            kind: HostKind::Local,
            endpoint: None,
            reachable: true,
            error: None,
            sessions: vec![session("work", 1)],
        },
        HostJson {
            name: "mini".to_owned(),
            label: "mini".to_owned(),
            kind: HostKind::Remote,
            endpoint: Some("quic://100.64.0.2:8788".to_owned()),
            reachable: true,
            error: None,
            sessions: Vec::new(),
        },
        HostJson {
            name: "xps".to_owned(),
            label: "xps".to_owned(),
            kind: HostKind::Remote,
            endpoint: Some("quic://100.64.0.3:8788".to_owned()),
            reachable: false,
            error: Some("did not answer within 3s".to_owned()),
            sessions: Vec::new(),
        },
    ]);
    assert_eq!(
        super::all_hosts::host_list_lines(&doc),
        [
            "laptop (this machine)",
            "  work: 2 running (1 client attached)",
            "mini",
            "  (no sessions)",
            "xps  unreachable: did not answer within 3s",
        ]
    );
}

pub(super) fn session(name: &str, windows: u16, clients: u16) -> SessionInfo {
    SessionInfo::new(SessionId::new(1), name)
        .with_window_count(windows)
        .with_attached_client_count(clients)
}

/// A session line ends in its key, so the listed prefix is what
/// `phux rm ^ab12cd` takes; a server without records prints none.
#[test]
fn a_session_line_ends_in_its_key() {
    let line = "work: 1 member (1 running), persistent".to_owned();
    assert_eq!(
        with_key(line.clone(), Some("^ab12cd")),
        "work: 1 member (1 running), persistent  ^ab12cd"
    );
    assert_eq!(with_key(line.clone(), None), line);
}

/// ADR-0105: a session with no windows says so, so a server that stays
/// up with zero processes explains itself.
#[test]
fn an_empty_session_is_marked() {
    assert_eq!(
        format_session_line(&session("parked", 0, 0).with_keep_empty(true)),
        "parked: 0 windows (empty)"
    );
    assert_eq!(
        format_session_line(&session("parked", 0, 1).with_keep_empty(true)),
        "parked: 0 windows (empty) (1 client attached)"
    );
    assert_eq!(
        format_session_line(&session("work", 1, 0).with_keep_empty(true)),
        "work: 1 window",
        "a populated keep-empty session is not marked empty"
    );
}

#[test]
fn empty_snapshot_yields_no_lines_and_the_empty_state_names_next_commands() {
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1));
    assert!(session_lines(&snapshot, None).is_empty());

    // The two-line empty state: what is missing, then the exact commands
    // that fix it (the `phux host ls` precedent).
    assert_eq!(EMPTY_STATE.len(), 2);
    assert_eq!(EMPTY_STATE[0], "No sessions.");
    assert!(EMPTY_STATE[1].contains("`phux`"));
    assert!(EMPTY_STATE[1].contains("`phux new NAME -- CMD`"));
}

/// `phux ls` shows each session's lifetime, member count, and the
/// members' execution states; a member-less session reads `empty`.
#[test]
fn rows_show_lifetime_members_and_execution_states() {
    use phux_core::activation::SessionLifetime;
    use phux_core::session_list::{ExecCounts, SessionJson};
    let row = |name: &str, lifetime, members, exec, windows| SessionJson {
        name: name.to_owned(),
        windows,
        attached: false,
        attached_clients: 0,
        keep_empty: lifetime == Some(SessionLifetime::Persistent),
        empty: members == 0,
        lifetime,
        members,
        exec,
        project: None,
        project_link: None,
        key: None,
    };
    assert_eq!(
        format_row(&row(
            "parked",
            Some(SessionLifetime::Persistent),
            0,
            ExecCounts::default(),
            0
        )),
        "parked: empty, persistent"
    );
    assert_eq!(
        format_row(&row(
            "work",
            Some(SessionLifetime::Ephemeral),
            4,
            ExecCounts {
                running: 2,
                completed: 1,
                lost: 1,
                ..ExecCounts::default()
            },
            2
        )),
        "work: 4 members (2 running, 1 completed, 1 lost), 2 windows, ephemeral"
    );
    assert_eq!(
        format_row(&row(
            "old",
            None,
            1,
            ExecCounts {
                running: 1,
                ..ExecCounts::default()
            },
            1
        )),
        "old: 1 member (1 running), 1 window",
        "a server that reports no lifetime gets none printed"
    );
}

#[test]
fn zero_clients_render_no_attachment_note() {
    assert_eq!(
        format_session_line(&session("work", 3, 0)),
        "work: 3 windows"
    );
}

#[test]
fn one_client_renders_a_singular_count() {
    assert_eq!(
        format_session_line(&session("work", 1, 1)),
        "work: 1 window (1 client attached)"
    );
}

#[test]
fn two_clients_render_the_real_count() {
    assert_eq!(
        format_session_line(&session("work", 2, 2)),
        "work: 2 windows (2 clients attached)"
    );
}

/// An agent session nests under its pane, under its session; panes with
/// no session child print nothing, so the plain listing is unchanged.
#[test]
fn agent_sessions_nest_under_their_pane() {
    use phux_protocol::ids::ResourceKind;
    use phux_protocol::wire::info::{AgentFacet, ResourceInfo, WindowInfo};

    let work = SessionId::new(1);
    let window = WindowId::new(10);
    let snapshot = SessionSnapshot::new(work, window, ResourceId::local(7))
        .with_sessions(vec![SessionInfo::new(work, "work").with_window_count(1)])
        .with_windows(vec![WindowInfo::new(window, work, "shell").with_index(0)])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(7), window, 80, 24),
            ResourceInfo::new(ResourceId::local(8), window, 80, 24),
            ResourceInfo::new(ResourceId::local(9), window, 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(ResourceId::local(7)))
                .with_agent(Some(AgentFacet::new("claude", "working"))),
        ]);
    assert_eq!(
        session_lines(&snapshot, None),
        vec![
            "work: 3 members (3 running), 1 window",
            "  @7",
            "    @9: agent session claude (working)",
        ]
    );
}

/// A snapshot from a hub: one local session, a live satellite with one
/// session, and one the hub could not reach.
pub(super) fn federated_snapshot() -> SessionSnapshot {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::{HostInventory, HostSessionInfo};

    let work = SessionId::new(1);
    SessionSnapshot::new(work, WindowId::new(10), ResourceId::local(7))
        .with_sessions(vec![
            SessionInfo::new(work, "work")
                .with_window_count(1)
                .with_active_window(Some(WindowId::new(10))),
        ])
        .with_windows(vec![
            phux_protocol::wire::info::WindowInfo::new(WindowId::new(10), work, "shell")
                .with_active_resource(Some(ResourceId::local(7))),
        ])
        .with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
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

/// A hub groups the listing by host; an unreachable satellite is marked,
/// not dropped.
#[test]
fn host_inventory_groups_the_listing() {
    assert_eq!(
        session_lines(&federated_snapshot(), None),
        vec![
            "This host",
            "  work: 1 member (1 running), 1 window",
            "edge",
            "  build: 2 windows, 3 panes (1 client attached)",
            "down (unreachable)",
        ]
    );
}

/// Without a host inventory the listing is the flat, pre-grouping one.
#[test]
fn no_host_inventory_keeps_the_flat_listing() {
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
        .with_sessions(vec![session("work", 1, 0)]);
    assert_eq!(
        session_lines(&snapshot, None),
        vec!["work: 0 members, 1 window"]
    );
}

/// A grouped listing keeps each satellite Terminal's `host/@N` selector
/// under the host it lives on.
#[test]
fn grouped_listing_keeps_satellite_terminal_selectors() {
    use phux_protocol::ids::SatelliteHost;

    let mut snapshot = federated_snapshot();
    snapshot
        .resources
        .push(phux_protocol::wire::info::ResourceInfo::new(
            ResourceId::satellite(SatelliteHost::new("edge"), 9),
            WindowId::new(3),
            80,
            24,
        ));
    assert_eq!(
        session_lines(&snapshot, None),
        vec![
            "This host",
            "  work: 1 member (1 running), 1 window",
            "edge",
            "  build: 2 windows, 3 panes (1 client attached)",
            "  edge/@9: satellite terminal",
            "down (unreachable)",
        ]
    );
}

/// The text-mode "satellite sessions are unavailable" note fires only for
/// a server that lists satellite Terminals.
#[test]
fn host_sessions_note_needs_satellite_terminals() {
    use phux_protocol::ids::SatelliteHost;

    let plain = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
        .with_sessions(vec![session("work", 1, 0)]);
    assert!(!super::all_hosts::has_satellite_terminals(&plain));
    let hub = plain.with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
        ResourceId::satellite(SatelliteHost::new("edge"), 9),
        WindowId::new(3),
        80,
        24,
    )]);
    assert!(super::all_hosts::has_satellite_terminals(&hub));
}

#[test]
fn lines_are_name_sorted() {
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
        .with_sessions(vec![session("beta", 1, 0), session("alpha", 1, 0)]);
    let lines = session_lines(&snapshot, None);
    assert_eq!(
        lines,
        vec!["alpha: 0 members, 1 window", "beta: 0 members, 1 window"]
    );
}
