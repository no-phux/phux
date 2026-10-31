use super::{RebuildError, WireResourceId, validate_upgrade_blob};
use crate::state::ServerState;
use crate::upgrade::blob::{
    AgentSessionBlob, BLOB_VERSION, Counters, LayoutBlob, PaneBlob, SessionBlob, SplitDirBlob,
    StateBlob, WindowBlob,
};
use phux_terminal::actor::TerminalActor;
use std::path::PathBuf;

/// One pane's tree walks into a blob with resolved links and a snapshot.
#[tokio::test(flavor = "current_thread")]
async fn build_upgrade_blob_captures_tree_and_snapshot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut state = ServerState::new();

            // A session/window/pane in the registry.
            let (sid, tid) = state.seed_session("main");
            let session_wire = state.idspace.intern_session(sid).get();
            let window_wire = state.intern_window_wire(sid).get();

            // A real (no-PTY) actor answering the upgrade request.
            let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            tokio::task::spawn_local(bundle.actor.run());
            let pane_wire = state
                .register_resource_handle(tid, handle, token)
                .local_id()
                .expect("local wire id");

            let blob = state.build_upgrade_blob(7).await;

            assert_eq!(blob.listener_fd, 7);
            assert_eq!(blob.sessions.len(), 2, "the session and the Inbox");
            assert_eq!(blob.windows.len(), 1, "only the session has a pane");
            assert_eq!(blob.panes.len(), 1);
            let org = blob.org.as_ref().expect("records ride the blob");
            assert_eq!(org.sessions.len(), 2);
            assert_eq!(org.resources.len(), 1);

            let s = blob
                .sessions
                .iter()
                .find(|s| s.name == "main")
                .expect("main");
            assert_eq!(s.wire_id, session_wire);
            assert_eq!(s.name, "main");
            assert_eq!(s.window_wire_ids, vec![window_wire]);
            assert_eq!(s.key, state.registry().session_key(sid));
            assert!(s.keep_empty, "a persistent session reads keep-empty");

            let w = &blob.windows[0];
            assert_eq!(w.wire_id, window_wire);
            assert_eq!(w.session_wire_id, session_wire);
            assert_eq!(w.pane_wire_ids, vec![pane_wire]);

            let p = &blob.panes[0];
            assert_eq!(p.wire_id, pane_wire);
            assert_eq!(p.window_wire_id, window_wire);
            assert_eq!(p.key, state.registry().resource_key(tid));
            assert_eq!((p.cols, p.rows), (20, 5));
            assert_eq!(p.master_fd, None, "no-PTY actor has no master fd");
            assert_eq!(p.child_pid, None);
            assert!(
                String::from_utf8_lossy(&p.vt_replay_bytes).contains("hello"),
                "pane snapshot should carry the actor's seeded text"
            );

            // Counters sit above every minted id.
            assert!(blob.counters.next_session_wire_id > session_wire);
            assert!(blob.counters.next_window_wire_id > window_wire);
            assert!(blob.counters.next_terminal_wire_id > pane_wire);
        })
        .await;
}

/// state → blob → rebuild → blob round-trips tree, ids, and counters.
#[tokio::test(flavor = "current_thread")]
async fn rebuild_from_blob_round_trips_the_tree() {
    let local = tokio::task::LocalSet::new();
    Box::pin(local.run_until(async {
        let mut state = ServerState::new();
        let (sid, tid) = state.seed_session("main");
        state.idspace.intern_session(sid);
        state.intern_window_wire(sid);
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        tokio::task::spawn_local(bundle.actor.run());
        state.register_resource_handle(tid, bundle.handle, bundle.token);

        // A persistent session with no members survives the round trip.
        let parked = state.seed_empty_session("parked").expect("parked");
        state.idspace.intern_session(parked);

        let blob = state.build_upgrade_blob(7).await;

        // Rebuild into a brand-new state, then re-emit a blob from it.
        let mut fresh = ServerState::new();
        // phux-1x9s.6: every pane's actor passes through the runtime's
        // wiring before it runs, as a fresh spawn's does.
        let rebuilt = fresh
            .rebuild_from_blob(&blob, |core, pane, scrollback| {
                crate::runtime::apps::terminal::upgrade::rebuild_pane(
                    core,
                    pane,
                    scrollback,
                    |pane, actor| (pane, actor.has_pty()),
                )
            })
            .expect("rebuild");
        assert_eq!(
            rebuilt.exit_watchers.len(),
            blob.panes.len(),
            "every rebuilt pane must return an exit receiver for the runtime watcher"
        );
        assert_eq!(
            rebuilt.wired_panes.len(),
            blob.panes.len(),
            "every rebuilt pane must be wired before its actor runs"
        );
        assert!(
            rebuilt
                .wired_panes
                .iter()
                .all(|(pane, (wired, _))| pane == wired),
            "the wiring sees the pane it is handed back for"
        );
        let blob2 = fresh.build_upgrade_blob(7).await;

        assert_eq!(blob.sessions, blob2.sessions, "sessions round-trip");
        assert_eq!(blob.org, blob2.org, "records round-trip unchanged");
        assert_eq!(fresh.org().authority(), state.org().authority());
        assert!(
            blob2.sessions.iter().all(|s| s.keep_empty),
            "persistent lifetimes round-trip"
        );
        assert!(
            blob2
                .sessions
                .iter()
                .any(|s| s.name == "parked" && s.window_wire_ids.is_empty()),
            "an empty keep-empty session survives with zero windows"
        );
        assert_eq!(blob.windows, blob2.windows, "windows + layout round-trip");
        assert_eq!(blob.counters, blob2.counters, "id allocators round-trip");
        assert_eq!(blob.panes.len(), blob2.panes.len());

        let (p1, p2) = (&blob.panes[0], &blob2.panes[0]);
        assert_eq!(p1.wire_id, p2.wire_id);
        assert_eq!(p1.window_wire_id, p2.window_wire_id);
        assert_eq!((p1.cols, p1.rows), (p2.cols, p2.rows));
        assert!(
            String::from_utf8_lossy(&p2.vt_replay_bytes).contains("hello"),
            "rebuilt pane should replay its seed snapshot"
        );
    }))
    .await;
}

/// ADR-0109: the instance token changes exactly when ids can repeat.
#[tokio::test(flavor = "current_thread")]
async fn the_instance_token_survives_an_upgrade_and_only_an_upgrade() {
    let local = tokio::task::LocalSet::new();
    Box::pin(local.run_until(async {
        let mut state = ServerState::new();
        let token = state.idspace.instance();
        let blob = state.build_upgrade_blob(7).await;
        assert_eq!(blob.counters.server_instance, Some(*token.as_bytes()));

        let mut upgraded = ServerState::new();
        assert_ne!(
            upgraded.idspace.instance(),
            token,
            "a cold start mints a new token"
        );
        upgraded
            .rebuild_from_blob(&blob, crate::runtime::apps::terminal::upgrade::unwired)
            .expect("rebuild");
        assert_eq!(upgraded.idspace.instance(), token, "an upgrade keeps it");

        let mut legacy = state.build_upgrade_blob(7).await;
        legacy.counters.server_instance = None;
        let mut from_legacy = ServerState::new();
        let fresh = from_legacy.idspace.instance();
        from_legacy
            .rebuild_from_blob(&legacy, crate::runtime::apps::terminal::upgrade::unwired)
            .expect("rebuild");
        assert_eq!(
            from_legacy.idspace.instance(),
            fresh,
            "nothing to restore: the fresh token stands"
        );
    }))
    .await;
}

/// The program's title, as an `OSC 2` a pane emitted before the upgrade.
const PROGRAM_TITLE: &str = "vim README.md";

/// One pane whose actor saw `seed`, with `user_title` on its registry
/// entry, carried through blob -> rebuild -> a second blob (a second
/// upgrade). Returns the twice-upgraded state.
#[allow(
    clippy::future_not_send,
    reason = "test helper driving pane actors on the current-thread LocalSet"
)]
async fn upgrade_twice(user_title: Option<&str>, seed: &[u8]) -> ServerState {
    let mut state = ServerState::new();
    let (sid, tid) = state.seed_session("main");
    state
        .registry_mut()
        .terminal_mut(tid)
        .expect("terminal")
        .title = user_title.map(ToOwned::to_owned);
    state.idspace.intern_session(sid);
    state.intern_window_wire(sid);
    let bundle = TerminalActor::new_with_seed(20, 5, seed).expect("new_with_seed");
    tokio::task::spawn_local(bundle.actor.run());
    state.register_resource_handle(tid, bundle.handle, bundle.token);

    let blob = state.build_upgrade_blob(7).await;
    let mut once = ServerState::new();
    once.rebuild_from_blob(&blob, crate::runtime::apps::terminal::upgrade::unwired)
        .expect("first rebuild");
    let blob = once.build_upgrade_blob(7).await;
    let mut twice = ServerState::new();
    twice
        .rebuild_from_blob(&blob, crate::runtime::apps::terminal::upgrade::unwired)
        .expect("second rebuild");
    twice
}

/// `GET_STATE`'s `title` for the state's only pane: the user-set title.
fn get_state_title(state: &mut ServerState) -> Option<String> {
    let sid = state
        .registry()
        .sessions()
        .map(|(id, _)| id)
        .next()
        .expect("a session");
    let snapshot = state.build_session_snapshot(sid).expect("snapshot");
    assert_eq!(snapshot.resources.len(), 1);
    snapshot.resources[0].title.clone()
}

/// `GET_SCREEN`'s `title` for the state's only pane: the engine's OSC title.
#[allow(
    clippy::future_not_send,
    reason = "test helper driving pane actors on the current-thread LocalSet"
)]
async fn get_screen_title(state: &ServerState) -> Option<String> {
    use phux_terminal::actor::TerminalResource as _;
    let handles = state.upgrade_handles();
    assert_eq!(handles.len(), 1);
    let (reply, rx) = tokio::sync::oneshot::channel();
    handles[0]
        .1
        .terminal()
        .expect("terminal handle")
        .screen
        .send(phux_terminal::actor::ScreenRequest {
            pane: 1,
            scrollback: None,
            cells: false,
            format: 0,
            reply,
        })
        .await
        .expect("send screen request");
    match rx.await.expect("screen reply") {
        phux_terminal::actor::ScreenReply::Projection(screen) => screen.title,
        other @ phux_terminal::actor::ScreenReply::TooLarge { .. } => {
            panic!("unexpected screen reply: {other:?}")
        }
    }
}

/// phux-xy9y: a pane no one named keeps no user title across upgrades,
/// while the title its program set still reads back from the engine.
#[tokio::test(flavor = "current_thread")]
async fn an_upgrade_keeps_the_program_title_out_of_the_user_title() {
    let local = tokio::task::LocalSet::new();
    Box::pin(local.run_until(async {
        let seed = format!("hello\x1b]2;{PROGRAM_TITLE}\x07");
        let mut state = upgrade_twice(None, seed.as_bytes()).await;
        assert_eq!(
            get_state_title(&mut state),
            None,
            "GET_STATE's user-set title must stay unset"
        );
        assert_eq!(
            get_screen_title(&state).await.as_deref(),
            Some(PROGRAM_TITLE),
            "GET_SCREEN's OSC title must still report the program's title"
        );
    }))
    .await;
}

/// phux-xy9y: a user-set title survives unchanged even when the program
/// set a different OSC title, and neither overwrites the other.
#[tokio::test(flavor = "current_thread")]
async fn an_upgrade_keeps_the_user_title_beside_the_program_title() {
    let local = tokio::task::LocalSet::new();
    Box::pin(local.run_until(async {
        let seed = format!("hello\x1b]2;{PROGRAM_TITLE}\x07");
        let mut state = upgrade_twice(Some("build"), seed.as_bytes()).await;
        assert_eq!(get_state_title(&mut state).as_deref(), Some("build"));
        assert_eq!(
            get_screen_title(&state).await.as_deref(),
            Some(PROGRAM_TITLE)
        );

        // No OSC title at all: still nothing in the engine, user title kept.
        let mut state = upgrade_twice(Some("build"), b"hello").await;
        assert_eq!(get_state_title(&mut state).as_deref(), Some("build"));
        assert_eq!(get_screen_title(&state).await, None);
    }))
    .await;
}

/// A replayed title cannot break out of its `OSC 2`.
#[test]
fn the_replayed_title_drops_control_characters() {
    assert_eq!(
        super::osc_title_sequence("a\x07b\x1b]0;c"),
        b"\x1b]2;ab]0;c\x1b\\".to_vec()
    );
}

mod merge;
mod validation;
