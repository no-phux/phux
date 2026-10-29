//! Unit tests for the glance and the one-session view.

use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use phux_protocol::{ResourceId, SessionId, WindowId};

use super::session_lines;
use super::tests::{federated_snapshot, session};

/// The glance: what needs you leads, then what runs; a persistent
/// lifetime, window counts, and keys are left to `-l`.
#[test]
fn a_glance_line_names_only_counts_and_exceptions() {
    use phux_client::glance::SessionGlance;
    let row = |name: &str| SessionGlance {
        name: name.to_owned(),
        ..SessionGlance::default()
    };
    assert_eq!(
        super::glance_rows::glance_text(&SessionGlance {
            waiting: 1,
            failed: 1,
            running: 5,
            ..row("api")
        }),
        "api: needs you (1 agent waiting, 1 failed), 5 running"
    );
    assert_eq!(
        super::glance_rows::glance_text(&SessionGlance {
            running: 12,
            attached_clients: 1,
            ..row("build")
        }),
        "build: 12 running (1 client attached)"
    );
    assert_eq!(
        super::glance_rows::glance_text(&SessionGlance {
            ephemeral: true,
            ..row("scratch")
        }),
        "scratch: empty, ephemeral"
    );
    assert_eq!(
        super::glance_rows::glance_text(&SessionGlance {
            done: 3,
            ..row("ci")
        }),
        "ci: 3 done"
    );
    assert_eq!(
        super::glance_rows::glance_text(&SessionGlance {
            not_started: 2,
            to_organize: Some(2),
            ..row("inbox")
        }),
        "inbox: 2 in Intake, 2 not started"
    );
    assert_eq!(
        super::glance_rows::glance_text(&SessionGlance {
            waiting: 1,
            running: 1,
            to_organize: Some(2),
            ..row("my queue")
        }),
        "my queue: needs you (1 agent waiting), 2 in Intake, 1 running"
    );
    assert_eq!(
        super::glance_rows::glance_text(&SessionGlance {
            to_organize: Some(0),
            ..row("inbox")
        }),
        "inbox: Intake, empty"
    );
}

/// A swarm: sessions that need you sort first and are never collapsed
/// for a quiet one; the rest fold into one count line, so a thousand
/// sessions print about twenty lines.
#[test]
fn a_swarm_collapses_its_quiet_sessions() {
    use phux_client::glance::SessionGlance;
    let mut rows: Vec<SessionGlance> = (0..1000)
        .map(|n| SessionGlance {
            name: format!("s{n:04}"),
            running: 1,
            ..SessionGlance::default()
        })
        .collect();
    rows[700].failed = 1;
    rows[900].waiting = 1;
    let lines = super::glance_rows::collapse(rows);
    assert_eq!(
        lines.len(),
        super::glance_rows::GLANCE_ROWS + 1,
        "{lines:?}"
    );
    assert!(lines[0].starts_with("s0700: needs you"), "{lines:?}");
    assert!(lines[1].starts_with("s0900: needs you"), "{lines:?}");
    assert_eq!(lines[2], "s0000: 1 running");
    assert_eq!(
        lines[super::glance_rows::GLANCE_ROWS],
        "+980 quiet sessions; `phux ls -l` lists every session"
    );

    let few: Vec<SessionGlance> = (0..3)
        .map(|n| SessionGlance {
            name: format!("s{n}"),
            ..SessionGlance::default()
        })
        .collect();
    assert_eq!(
        super::glance_rows::collapse(few).len(),
        3,
        "no count line when it fits"
    );
}

/// A session linked into a project by name glances under that
/// project's header beside its git sessions; a link naming no
/// identity groups on its own.
#[test]
fn a_linked_session_glances_under_its_project() {
    use phux_client::glance::SessionGlance;
    use phux_core::project_identity::SessionProject;
    let phux = SessionProject {
        id: "github.com/no-phux/phux".to_owned(),
        name: "phux".to_owned(),
        checkout: "/w".to_owned(),
        branch: None,
        linked_worktree: false,
        host: Some("mini".to_owned()),
    };
    let glance = |name: &str| SessionGlance {
        name: name.to_owned(),
        running: 1,
        ..SessionGlance::default()
    };
    let row = |identity, link: Option<&str>| super::glance_rows::RowProject {
        identity,
        link: link.map(str::to_owned),
    };
    let lines = super::glance_rows::collapse_by_project(vec![
        (glance("main"), row(Some(phux), None)),
        (glance("notes"), row(None, Some("phux"))),
        (glance("ops-a"), row(None, Some("ops"))),
        (glance("ops-b"), row(None, Some("ops"))),
    ]);
    assert_eq!(
        lines,
        [
            "phux (project)",
            "  main: 1 running",
            "  notes: 1 running",
            "ops (project)",
            "  ops-a: 1 running",
            "  ops-b: 1 running",
        ]
    );
}

/// ADR-0168: sessions of one repository glance under one header, the
/// primary checkout first and its linked worktrees indented beneath;
/// a project with one session stays a plain line.
#[test]
fn the_glance_groups_sessions_by_project() {
    use phux_client::glance::SessionGlance;
    use phux_core::project_identity::SessionProject;
    let project = |linked| SessionProject {
        id: "github.com/no-phux/phux".to_owned(),
        name: "phux".to_owned(),
        checkout: "/w".to_owned(),
        branch: None,
        linked_worktree: linked,
        host: Some("mini".to_owned()),
    };
    let lone = SessionProject {
        id: "github.com/a/other".to_owned(),
        ..project(false)
    };
    let (main, feat) = (project(false), project(true));
    let glance = |name: &str, waiting| SessionGlance {
        name: name.to_owned(),
        waiting,
        running: 1,
        ..SessionGlance::default()
    };
    let of = |identity| super::glance_rows::RowProject {
        identity,
        link: None,
    };
    let lines = super::glance_rows::collapse_by_project(vec![
        (glance("feat-auth", 1), of(Some(feat))),
        (glance("scratch", 0), of(None)),
        (glance("phux", 0), of(Some(main))),
        (glance("other", 0), of(Some(lone))),
    ]);
    assert_eq!(
        lines,
        [
            "phux (project, 1 need you)",
            "  phux: 1 running",
            "    feat-auth: needs you (1 agent waiting), 1 running",
            "other: 1 running",
            "scratch: 1 running",
        ]
    );
}

/// A hub's satellite glances like this host: its sessions that need
/// you first, the quiet rest folded into one count, its terminals left
/// to `-l`, which still prints every session and terminal.
#[test]
fn a_satellite_swarm_collapses_like_this_host() {
    use std::collections::HashMap;

    use phux_client::glance::Sources;
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::frame::ResourceLifecycle;
    use phux_protocol::wire::info::{ExitFacet, HostInventory, HostSessionInfo, ResourceInfo};

    let edge = SatelliteHost::new("edge");
    let sessions = (0..500)
        .map(|n| {
            HostSessionInfo::new(SessionId::new(n), format!("s{n:03}"))
                .with_window_count(1)
                .with_pane_count(1)
        })
        .collect();
    let mut snapshot = federated_snapshot().with_hosts(vec![
        HostInventory::reachable(edge.clone(), sessions),
        HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
    ]);
    snapshot.resources.extend((0..500).map(|n| {
        let pane = ResourceInfo::new(
            ResourceId::satellite(edge.clone(), n),
            WindowId::new(0),
            80,
            24,
        )
        .with_session(Some(SessionId::new(n)));
        if n == 321 {
            pane.with_lifecycle(ResourceLifecycle::Exited)
                .with_exit(Some(ExitFacet::new(0, 0).with_exit_status(Some(2))))
        } else {
            pane
        }
    }));
    let agents = HashMap::new();
    let sources = Sources {
        snapshot: &snapshot,
        organization: None,
        agents: &agents,
    };
    let lines = super::glance_rows::glance_lines(sources);
    assert_eq!(
        lines[..4],
        [
            "This host",
            "  work: 1 running",
            "edge",
            "  s321: needs you (1 failed)",
        ],
        "{lines:?}"
    );
    assert_eq!(lines[4], "  s000: 1 running");
    assert_eq!(
        lines[3 + super::glance_rows::GLANCE_ROWS],
        "  +480 quiet sessions; `phux ls -l` lists every session"
    );
    assert_eq!(
        lines[4 + super::glance_rows::GLANCE_ROWS],
        "down (unreachable)"
    );
    assert_eq!(lines.len(), 5 + super::glance_rows::GLANCE_ROWS);

    let detail = session_lines(&snapshot, None);
    assert_eq!(detail.len(), 2 + 1 + 500 + 500 + 1, "-l keeps every line");
}

/// A hub that does not say which satellite session a terminal is in
/// glances each satellite's terminals as one counted line.
#[test]
fn unjoined_satellite_terminals_glance_as_one_line_per_host() {
    use std::collections::HashMap;

    use phux_client::glance::Sources;
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::frame::ResourceLifecycle;
    use phux_protocol::wire::info::{ExitFacet, ResourceInfo};

    let edge = SatelliteHost::new("edge");
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
        .with_sessions(vec![session("work", 1, 0)])
        .with_resources(
            (0..300)
                .map(|n| {
                    let pane = ResourceInfo::new(
                        ResourceId::satellite(edge.clone(), n),
                        WindowId::new(0),
                        80,
                        24,
                    );
                    if n == 7 {
                        pane.with_lifecycle(ResourceLifecycle::Exited)
                            .with_exit(Some(ExitFacet::new(0, 0).with_exit_status(Some(1))))
                    } else {
                        pane
                    }
                })
                .collect(),
        );
    let agents = HashMap::new();
    let sources = Sources {
        snapshot: &snapshot,
        organization: None,
        agents: &agents,
    };
    assert_eq!(
        super::glance_rows::glance_lines(sources),
        [
            "work: empty",
            "edge terminals: needs you (1 failed), 299 running"
        ]
    );
    assert_eq!(session_lines(&snapshot, None).len(), 301);
}

/// The session view lists each terminal, exceptions first: an agent
/// blocked on a person, then the failure, then quiet work.
#[test]
fn a_session_view_lists_terminals_exceptions_first() {
    use std::collections::HashMap;

    use phux_client::agent_meta::{AgentMetaState, AgentRecord};
    use phux_client::glance::Sources;
    use phux_protocol::wire::frame::ResourceLifecycle;
    use phux_protocol::wire::info::{ExitFacet, ResourceInfo, WindowInfo};

    let work = SessionId::new(1);
    let window = WindowId::new(10);
    let failed = ResourceInfo::new(ResourceId::local(9), window, 80, 24)
        .with_lifecycle(ResourceLifecycle::Exited)
        .with_exit(Some(ExitFacet::new(0, 0).with_exit_status(Some(1))));
    let snapshot = SessionSnapshot::new(work, window, ResourceId::local(3))
        .with_sessions(vec![SessionInfo::new(work, "api").with_window_count(1)])
        .with_windows(vec![WindowInfo::new(window, work, "shell").with_index(0)])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(3), window, 80, 24)
                .with_title(Some("zsh".to_owned())),
            ResourceInfo::new(ResourceId::local(7), window, 80, 24),
            failed,
        ]);
    let agents = HashMap::from([(
        ResourceId::local(7),
        AgentRecord {
            name: "claude".to_owned(),
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        },
    )]);
    let sources = Sources {
        snapshot: &snapshot,
        organization: None,
        agents: &agents,
    };
    assert_eq!(
        super::glance_rows::glance_lines(sources),
        ["api: needs you (1 agent waiting, 1 failed), 1 running"]
    );
    assert_eq!(
        super::session_view::one_session_lines(sources, &snapshot.sessions[0], false),
        [
            "api: needs you (1 agent waiting, 1 failed), 1 running",
            "  @7  waiting          claude blocked",
            "  @9  failed (exit 1)",
            "  @3  running                          zsh",
        ]
    );
}

/// Program-set text cannot smuggle control or bidi characters into the
/// listing, and the home directory reads `~`.
#[test]
fn titles_and_directories_print_safely() {
    assert_eq!(
        super::session_view::printable("a\u{1b}[2Jb\u{202e}c"),
        "a[2Jbc"
    );
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        assert_eq!(
            super::session_view::home_relative(&format!("{home}/src")),
            "~/src"
        );
        assert_eq!(
            super::session_view::home_relative(&format!("{home}x")),
            format!("{home}x")
        );
    }
}
