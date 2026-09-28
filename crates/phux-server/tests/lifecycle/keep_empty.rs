//! ADR-0105/0114/0129: a keep-empty session survives its last window, and
//! every session removal path cleans up its layout keys. Each server keeps a
//! pre-seeded `anchor` session, so these assertions are about the session,
//! not the server's own last-session exit (`server_self_exit.rs`).

use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    Command, CommandResult, DetachReason, FrameKind, SESSION_KEEP_EMPTY_KEY, Scope, SpawnResult,
    encode_session_keep_empty,
};
use phux_protocol::wire::info::SessionInfo;
use phux_server_testkit::{command, send_frame, spawn_resource};
use tokio::net::UnixStream;

use crate::common::{
    Server, attach, create, create_result, create_seeded, get_metadata, send_create, session, sh,
    state, token, wait_for_state, wait_frame,
};

const ANCHOR: &str = "anchor";

async fn start() -> (Server, UnixStream) {
    let server = Server::pty(Some(ANCHOR));
    let conn = server.connect().await;
    (server, conn)
}

async fn set_keep_empty(stream: &mut UnixStream, name: &str, keep: bool, request_id: u32) {
    let set = FrameKind::SetMetadata {
        request_id,
        scope: Scope::Global,
        key: SESSION_KEEP_EMPTY_KEY.to_owned(),
        value: encode_session_keep_empty(name, keep),
    };
    send_frame(stream, &set).await;
}

async fn command_ok(stream: &mut UnixStream, request_id: u32, cmd: Command) {
    let result = command(stream, request_id, cmd).await;
    assert!(
        matches!(result, CommandResult::Ok),
        "command failed: {result:?}"
    );
}

const fn kill(pane: ResourceId) -> Command {
    Command::KillResource {
        terminal_id: pane,
        operation_id: None,
    }
}

/// A client attached to `name` and subscribed to the keep-empty mark.
async fn attached_watcher(server: &Server, name: &str) -> UnixStream {
    let mut client = server.connect().await;
    attach(&mut client, name).await;
    let subscribe = FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: SESSION_KEEP_EMPTY_KEY.to_owned(),
    };
    send_frame(&mut client, &subscribe).await;
    state(&mut client, 50).await;
    client
}

/// Read to `DETACHED`, returning its reason and whether the released mark
/// was broadcast first.
async fn released_then_detached(
    client: &mut UnixStream,
    name: &str,
) -> (bool, Option<DetachReason>) {
    let mut released = false;
    let reason = wait_frame(client, "DETACHED", |frame| match frame {
        FrameKind::MetadataChanged { key, value, .. } if key == SESSION_KEEP_EMPTY_KEY => {
            assert_eq!(value, Some(encode_session_keep_empty(name, false)));
            released = true;
            None
        }
        FrameKind::Detached { reason, .. } => Some(reason),
        _ => None,
    })
    .await;
    (released, reason)
}

/// A group kill of a keep-empty session broadcasts the released mark, then
/// detaches its attached client with `SESSION_KILLED`.
#[test]
fn group_kill_detaches_clients_attached_to_a_keep_empty_session() {
    phux_server_testkit::run_local(async {
        let (server, mut conn) = start().await;
        let pane = create_seeded(&mut conn, 1, "kept", true).await;
        let mut client = attached_watcher(&server, "kept").await;

        let kill_all = Command::KillResources {
            ids: vec![pane],
            operation_id: None,
        };
        command_ok(&mut conn, 2, kill_all).await;
        assert_eq!(
            released_then_detached(&mut client, "kept").await,
            (true, Some(DetachReason::SessionKilled))
        );
        wait_for_state(&mut conn, 100, "the session to go", |s| {
            session(s, "kept").is_none()
        })
        .await;

        drop((client, conn));
        server.stop().await;
    });
}

/// Close Tab of a keep-empty session's last pane leaves it listed and empty
/// for a second attached client; clearing the mark (End Session) then reaps
/// it and detaches that client.
#[test]
fn close_tab_leaves_keep_empty_visible_until_the_mark_is_cleared() {
    phux_server_testkit::run_local(async {
        let (server, mut conn) = start().await;
        let pane = create_seeded(&mut conn, 1, "kept", true).await;
        let mut client = attached_watcher(&server, "kept").await;

        command_ok(&mut conn, 2, Command::CloseTabResources { ids: vec![pane] }).await;
        wait_for_state(&mut conn, 100, "the last window to go", |s| {
            session(s, "kept").is_some_and(SessionInfo::is_empty)
        })
        .await;
        let listing = state(&mut client, 51).await;
        let kept = session(&listing, "kept").expect("the second client still sees it");
        assert!(kept.keep_empty && kept.is_empty());

        set_keep_empty(&mut conn, "kept", false, 3).await;
        assert_eq!(
            released_then_detached(&mut client, "kept").await.1,
            Some(DetachReason::SessionKilled)
        );
        wait_for_state(&mut conn, 200, "the session to go", |s| {
            session(s, "kept").is_none()
        })
        .await;

        drop((client, conn));
        server.stop().await;
    });
}

/// The reap cascade stops at a keep-empty session when its last pane dies;
/// a default session goes with its pane; a `KILL_RESOURCES` naming every
/// pane of a keep-empty session is a group teardown that removes it.
#[test]
fn reaps_stop_at_keep_empty_sessions_except_a_group_kill() {
    phux_server_testkit::run_local(async {
        let (server, mut conn) = start().await;
        let kept = create_seeded(&mut conn, 1, "kept", true).await;
        let plain = create_seeded(&mut conn, 2, "plain", false).await;
        let grouped = create_seeded(&mut conn, 3, "grouped", true).await;
        command_ok(&mut conn, 4, kill(kept)).await;
        command_ok(&mut conn, 5, kill(plain)).await;
        let kill_all = Command::KillResources {
            ids: vec![grouped],
            operation_id: None,
        };
        command_ok(&mut conn, 6, kill_all).await;

        let snapshot = wait_for_state(&mut conn, 100, "all three reaps", |s| {
            session(s, "plain").is_none()
                && session(s, "grouped").is_none()
                && session(s, "kept").is_some_and(SessionInfo::is_empty)
        })
        .await;
        let kept = session(&snapshot, "kept").unwrap();
        assert!(kept.keep_empty);
        assert_eq!(kept.window_count, 0);

        drop(conn);
        server.stop().await;
    });
}

/// Every way a session loses its last window or goes away deletes the
/// `<prefix>.layout/v1/<id>` keys naming it (L3 §3.2/§3.5): the default TUI
/// key and a named projection alike, whether the session is keep-empty
/// (window closes), ordinary (reaped), or parked and then cleared.
#[test]
fn every_removal_path_deletes_the_sessions_layout_keys() {
    phux_server_testkit::run_local(async {
        let (server, mut conn) = start().await;
        let scope = Scope::Group(GroupId::new(1));
        let kept = create_seeded(&mut conn, 1, "kept", true).await;
        let gone = create_seeded(&mut conn, 2, "gone", false).await;
        let listing = state(&mut conn, 3).await;
        let keys = |name: &str| {
            let id = session(&listing, name).unwrap().id.get();
            [
                format!("phux.tui.layout/v1/{id}"),
                format!("myapp.layout/v1/{id}"),
            ]
        };
        let (kept_keys, gone_keys) = (keys("kept"), keys("gone"));
        for (request_id, key) in (10..).zip(kept_keys.iter().chain(&gone_keys)) {
            let set = FrameKind::SetMetadata {
                request_id,
                scope: scope.clone(),
                key: key.clone(),
                value: b"stale tree".to_vec(),
            };
            send_frame(&mut conn, &set).await;
        }
        assert!(
            get_metadata(&mut conn, 20, scope.clone(), &gone_keys[1])
                .await
                .is_some()
        );

        command_ok(&mut conn, 21, kill(kept)).await;
        command_ok(&mut conn, 22, kill(gone)).await;
        wait_for_state(&mut conn, 100, "both reaps", |s| {
            session(s, "gone").is_none() && session(s, "kept").is_some_and(SessionInfo::is_empty)
        })
        .await;
        for (n, key) in (30..).zip(kept_keys.iter().chain(&gone_keys)) {
            assert!(
                get_metadata(&mut conn, n, scope.clone(), key)
                    .await
                    .is_none(),
                "{key} survived"
            );
        }

        // A key written while `kept` sits parked goes when the cleared mark
        // reaps it through `reap_session_if_empty`.
        let parked_write = FrameKind::SetMetadata {
            request_id: 40,
            scope: scope.clone(),
            key: kept_keys[1].clone(),
            value: b"parked-write".to_vec(),
        };
        send_frame(&mut conn, &parked_write).await;
        assert!(
            get_metadata(&mut conn, 41, scope.clone(), &kept_keys[1])
                .await
                .is_some()
        );
        set_keep_empty(&mut conn, "kept", false, 42).await;
        wait_for_state(&mut conn, 200, "the cleared session to go", |s| {
            session(s, "kept").is_none()
        })
        .await;
        assert!(
            get_metadata(&mut conn, 43, scope, &kept_keys[1])
                .await
                .is_none()
        );

        drop(conn);
        server.stop().await;
    });
}

/// `empty: true` creates a windowless keep-empty session (and refuses a
/// `command`); it is attachable with sentinel focus ids, a spawn from that
/// attach opens its first window, and clearing the mark on an empty session
/// removes it.
#[test]
fn empty_sessions_are_created_attached_filled_and_cleared() {
    phux_server_testkit::run_local(async {
        let (server, mut conn) = start().await;
        let result = create(
            &mut conn,
            1,
            serde_json::json!({ "name": "parked", "empty": true }),
        )
        .await;
        assert_eq!(result["name"], "parked");
        assert!(result["terminal_id"].is_null());
        assert_eq!(result["empty"], true);
        let snapshot = state(&mut conn, 2).await;
        let parked = session(&snapshot, "parked").unwrap();
        assert!(parked.keep_empty && parked.is_empty() && parked.window_count == 0);
        assert!(!session(&snapshot, ANCHOR).unwrap().keep_empty);

        let body = serde_json::json!({
            "name": "confused",
            "empty": true,
            "command": ["/bin/sh", "-c", "read _"],
            "request_token": token(3),
        });
        send_create(&mut conn, 3, &body).await;
        assert!(create_result(&mut conn, 4, &token(3)).await.is_none());
        assert!(session(&state(&mut conn, 5).await, "confused").is_none());

        let mut client = server.connect().await;
        let snapshot = attach(&mut client, "parked").await;
        assert_eq!(snapshot.focused_resource, ResourceId::local(0));
        assert_eq!(
            snapshot.focused_session,
            session(&snapshot, "parked").unwrap().id
        );
        let result = spawn_resource(&mut client, 7, sh("read _")).await;
        assert!(matches!(result, SpawnResult::Ok(_)), "{result:?}");
        assert_eq!(
            session(&state(&mut conn, 6).await, "parked")
                .unwrap()
                .window_count,
            1
        );

        create(
            &mut conn,
            8,
            serde_json::json!({ "name": "doomed", "empty": true }),
        )
        .await;
        set_keep_empty(&mut conn, "doomed", false, 9).await;
        // Frames are handled in order, so this sees the write.
        assert!(session(&state(&mut conn, 10).await, "doomed").is_none());

        drop((client, conn));
        server.stop().await;
    });
}

/// `phux.session.keep_empty/v1` sets the mark, broadcasts only a change, and
/// is never stored.
#[test]
fn keep_empty_mark_is_applied_and_broadcast_on_change() {
    phux_server_testkit::run_local(async {
        let (server, mut writer) = start().await;
        let mut watcher = server.connect().await;
        let subscribe = FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: SESSION_KEEP_EMPTY_KEY.to_owned(),
        };
        send_frame(&mut watcher, &subscribe).await;
        state(&mut watcher, 1).await;

        create_seeded(&mut writer, 1, "work", false).await;
        set_keep_empty(&mut writer, "work", true, 2).await;
        set_keep_empty(&mut writer, "work", true, 3).await; // no change, no broadcast
        assert!(
            session(&state(&mut writer, 4).await, "work")
                .unwrap()
                .keep_empty
        );
        set_keep_empty(&mut writer, "work", false, 5).await;

        let mut seen = Vec::new();
        wait_frame(&mut watcher, "both changes", |frame| {
            if let FrameKind::MetadataChanged { key, value, .. } = frame
                && key == SESSION_KEEP_EMPTY_KEY
            {
                seen.push(value);
            }
            (seen.len() == 2).then_some(())
        })
        .await;
        assert_eq!(
            seen,
            [
                Some(encode_session_keep_empty("work", true)),
                Some(encode_session_keep_empty("work", false)),
            ]
        );
        assert!(
            get_metadata(&mut writer, 6, Scope::Global, SESSION_KEEP_EMPTY_KEY)
                .await
                .is_none(),
            "the mark is applied, never stored"
        );

        drop((watcher, writer));
        server.stop().await;
    });
}
