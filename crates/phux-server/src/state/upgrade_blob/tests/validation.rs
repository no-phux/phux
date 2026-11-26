//! Refusing a malformed blob before anything mutates.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

pub(super) fn empty_counters() -> Counters {
    Counters {
        next_session_wire_id: 10,
        next_terminal_wire_id: 10,
        next_window_wire_id: 10,
        next_touch_timestamp: 1,
        server_instance: None,
    }
}

pub(super) fn session(wire_id: u32, windows: Vec<u32>) -> SessionBlob {
    SessionBlob {
        wire_id,
        name: "main".to_owned(),
        window_wire_ids: windows,
        active_window: None,
        created_at_unix_nanos: 0,
        last_touched: None,
        root: None,
        keep_empty: false,
        key: None,
    }
}

pub(super) fn window(wire_id: u32, session_wire_id: u32, panes: Vec<u32>) -> WindowBlob {
    WindowBlob {
        wire_id,
        session_wire_id,
        pane_wire_ids: panes,
        active_resource: None,
        layout: None,
        last_cwd: None,
    }
}

pub(super) fn no_pty_pane(wire_id: u32, window_wire_id: u32) -> PaneBlob {
    PaneBlob {
        wire_id,
        window_wire_id,
        cols: 80,
        rows: 24,
        cell_px: None,
        cwd: PathBuf::from("/tmp"),
        title: None,
        osc_title: None,
        term: "xterm-256color".to_owned(),
        child_pid: None,
        master_fd: None,
        vt_replay_bytes: Vec::new(),
        scrollback_bytes: Vec::new(),
        retained_exit: None,
        retain_secs: None,
        asked: None,
        key: None,
        program_resume_token: None,
    }
}

pub(super) fn blob(
    sessions: Vec<SessionBlob>,
    windows: Vec<WindowBlob>,
    panes: Vec<PaneBlob>,
) -> StateBlob {
    StateBlob {
        version: BLOB_VERSION,
        listener_fd: 7,
        counters: empty_counters(),
        sessions,
        windows,
        panes,
        agent_sessions: Vec::new(),
        metadata: Vec::new(),
        org: None,
        delegations: Vec::new(),
        native_agents: Vec::new(),
        documents: Vec::new(),
    }
}

/// Session 1, its window 2, and its no-PTY pane 3.
pub(super) fn one_pane_blob() -> StateBlob {
    blob(
        vec![session(1, vec![2])],
        vec![window(2, 1, vec![3])],
        vec![no_pty_pane(3, 2)],
    )
}

pub(super) fn session_names(state: &ServerState) -> Vec<String> {
    let mut names: Vec<String> = state
        .registry()
        .sessions()
        .map(|(_, s)| s.name.clone())
        .collect();
    names.sort();
    names
}

/// A complete blob from an image that predates the records rebuilds as
/// an import: its session joins the records already here, awaiting
/// organization, with its pane as a running member arranged as its
/// window was.
#[tokio::test(flavor = "current_thread")]
async fn rebuild_from_a_complete_blob_is_transactional() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut state = ServerState::new();
            let _ = state.seed_empty_session("already-here");
            let handoff = one_pane_blob();
            validate_upgrade_blob(&handoff).expect("complete topology");
            crate::runtime::apps::terminal::test_support::rebuild_unwired(&mut state, &handoff)
                .expect("rebuild");
            assert_eq!(
                session_names(&state),
                vec![
                    "already-here".to_owned(),
                    "intake".to_owned(),
                    "main".to_owned()
                ]
            );
            let main = state.sessions().by_name("main").expect("main").clone();
            assert!(
                main.awaiting_organization,
                "imported work awaits organization"
            );
            assert_eq!(main.lifetime, phux_core::SessionLifetime::Ephemeral);
            assert_eq!(main.members.len(), 1);
            let pane = state
                .idspace
                .resource_from_wire(&WireResourceId::local(3))
                .expect("pane under its wire id");
            let record = state.registry().record(pane).expect("record");
            let activation = record.activation.as_ref().expect("activation 1");
            assert_eq!(activation.id, phux_core::ActivationId::FIRST);
            assert_eq!(activation.state, phux_core::ExecState::Running);
            let arrangement = state
                .org()
                .arrangement(&main.arrangement.expect("arranged"))
                .expect("arrangement");
            assert_eq!(arrangement.groups.len(), 1, "one group per window");
            assert_eq!(arrangement.resources(), vec![record.key]);
            assert!(
                state.org().check_invariants().is_ok(),
                "the import keeps every invariant"
            );
            // phux-lv57: a resumed tree has served no client in this
            // process, so it must not arm last-session self-exit.
            assert!(!state.lifecycle().has_served_client());
        })
        .await;
}

/// An old image's odd shapes still import: a session whose only window
/// lost its panes stays as a persistent record instead of an empty
/// ephemeral one, and a split banked at an edge ratio is clamped into
/// the open interval the arrangement holds.
#[tokio::test(flavor = "current_thread")]
async fn import_keeps_empty_sessions_and_clamps_edge_ratios() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut state = ServerState::new();
            let mut empty = session(1, vec![2]);
            empty.name = "empty".to_owned();
            let mut split = window(5, 4, vec![6, 7]);
            split.layout = Some(LayoutBlob::Split {
                dir: SplitDirBlob::Vertical,
                ratio: 1.0,
                left: Box::new(LayoutBlob::Leaf(6)),
                right: Box::new(LayoutBlob::Leaf(7)),
            });
            let handoff = blob(
                vec![empty, session(4, vec![5])],
                vec![window(2, 1, Vec::new()), split],
                vec![no_pty_pane(6, 5), no_pty_pane(7, 5)],
            );
            crate::runtime::apps::terminal::test_support::rebuild_unwired(&mut state, &handoff)
                .expect("an old image with an emptied window imports");
            let empty = state.sessions().by_name("empty").expect("empty kept");
            assert_eq!(empty.lifetime, phux_core::SessionLifetime::Persistent);
            let main = state.sessions().by_name("main").expect("main").clone();
            let arrangement = state
                .org()
                .arrangement(&main.arrangement.expect("arranged"))
                .expect("arrangement");
            match &arrangement.groups[0].tree {
                phux_core::LayoutNode::Split { ratio, .. } => {
                    assert!(*ratio > 0.0 && *ratio < 1.0, "ratio {ratio}");
                }
                other @ phux_core::LayoutNode::Leaf(_) => {
                    panic!("expected the split, got {other:?}")
                }
            }
        })
        .await;
}

/// A session that names a window the blob does not contain fails closed
/// and leaves the destination state untouched.
#[tokio::test(flavor = "current_thread")]
async fn dangling_window_id_does_not_mutate_state() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut state = ServerState::new();
            let _ = state.seed_empty_session("already-here");
            let handoff = blob(vec![session(1, vec![99])], Vec::new(), Vec::new());
            match crate::runtime::apps::terminal::test_support::rebuild_unwired(
                &mut state, &handoff,
            ) {
                Err(RebuildError::DanglingRef { kind, id }) => {
                    assert_eq!(kind, "window");
                    assert_eq!(id, 99);
                }
                other => panic!("expected dangling window, got {other:?}"),
            }
            assert_eq!(
                session_names(&state),
                vec!["already-here".to_owned(), "intake".to_owned()]
            );
        })
        .await;
}

pub(super) fn agent_session(wire_id: u32, parent_wire_id: u32) -> AgentSessionBlob {
    AgentSessionBlob {
        wire_id,
        parent_wire_id,
        provider: "claude".to_owned(),
        native_id: None,
        state: None,
        base_seq: 1,
        dropped: 0,
        ended: false,
        records: vec!["{\"seq\":1,\"ts_ms\":1,\"type\":\"prompt\",\"data\":{}}\n".to_owned()],
        key: None,
        native_publications: Vec::new(),
        native_publication_high_water: 0,
    }
}

/// Duplicate session ids, an agent session reusing a pane's id (one
/// wire-id space), and half a PTY pair are refused.
#[test]
pub(super) fn malformed_ids_and_pty_pairs_are_rejected() {
    let duplicate = blob(
        vec![session(1, Vec::new()), session(1, Vec::new())],
        Vec::new(),
        Vec::new(),
    );
    assert!(matches!(
        validate_upgrade_blob(&duplicate),
        Err(RebuildError::DuplicateId {
            kind: "session",
            id: 1
        })
    ));
    let mut reused = one_pane_blob();
    reused.agent_sessions = vec![agent_session(3, 3)];
    assert!(matches!(
        validate_upgrade_blob(&reused),
        Err(RebuildError::DuplicateId {
            kind: "resource",
            id: 3
        })
    ));
    let mut half_pty = one_pane_blob();
    half_pty.panes[0].master_fd = Some(11);
    assert!(matches!(
        validate_upgrade_blob(&half_pty),
        Err(RebuildError::InvalidPtyPair { wire_id: 3 })
    ));
}

/// A session whose parent pane is not in the blob is dropped; the rest
/// of the tree, and every other session, still resumes.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_session_without_its_parent_pane_is_dropped() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut state = ServerState::new();
            let mut handoff = one_pane_blob();
            handoff.agent_sessions = vec![agent_session(4, 3), agent_session(5, 99)];
            let watchers =
                crate::runtime::apps::terminal::test_support::rebuild_unwired(&mut state, &handoff)
                    .expect("rebuild")
                    .exit_watchers;
            assert_eq!(watchers.len(), 2, "the pane and its one session");
            let kept = state.idspace.resource_from_wire(&WireResourceId::local(4));
            assert!(kept.is_some(), "the bound session resumes under its id");
            assert!(
                state
                    .idspace
                    .resource_from_wire(&WireResourceId::local(5))
                    .is_none(),
                "the orphaned session is dropped"
            );
        })
        .await;
}
