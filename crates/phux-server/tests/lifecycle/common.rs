//! Helpers shared by the lifecycle suites.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::path::PathBuf;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, EventStamp, FrameKind,
    SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY_PREFIX, Scope, SpawnResult, StateScope,
    ViewportInfo,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};
use phux_server::{ServerConfig, ServerError};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, WIRE_RECV_TIMEOUT, command, join_after_shutdown,
    recv_until_deadline, send_frame, spawn_resource, spawn_server_with, wait_for_socket,
};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// A server in its own temp dir.
pub(crate) struct Server {
    pub(crate) tmp: TempDir,
    pub(crate) socket: PathBuf,
    shutdown: oneshot::Sender<()>,
    handle: JoinHandle<Result<(), ServerError>>,
}

impl Server {
    /// Start a server, optionally pre-seeding session `pre_seeded`.
    pub(crate) fn start(
        pre_seeded: Option<&str>,
        configure: impl FnOnce(&mut ServerConfig),
    ) -> Self {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, handle) = spawn_server_with(socket.clone(), pre_seeded, configure);
        Self {
            tmp,
            socket,
            shutdown,
            handle,
        }
    }

    /// A server whose panes (seed and attach-create) run on real PTYs.
    pub(crate) fn pty(pre_seeded: Option<&str>) -> Self {
        Self::start(pre_seeded, |cfg| cfg.seed_with_pty = true)
    }

    /// A HELLO'd client.
    pub(crate) async fn connect(&self) -> UnixStream {
        wait_for_socket(&self.socket, SOCKET_CONNECT_DEADLINE).await
    }

    pub(crate) async fn stop(self) {
        join_after_shutdown(self.shutdown, self.handle).await;
    }
}

/// `/bin/sh -c script` as an argv.
pub(crate) fn sh(script: &str) -> Spawn {
    Spawn::command(&["/bin/sh", "-c", script])
}

/// An unmodified Enter press with no text (the encoder synthesizes the CR).
pub(crate) const fn enter_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Send Enter to `pane`: releases children blocked on `read _`, so their
/// output or exit cannot race the spawn reply.
pub(crate) async fn release(stream: &mut UnixStream, pane: &ResourceId) {
    let key = FrameKind::InputKey {
        terminal_id: pane.clone(),
        event: enter_key(),
    };
    send_frame(stream, &key).await;
}

/// Skip frames until `pred` matches, failing after [`WIRE_RECV_TIMEOUT`].
pub(crate) async fn wait_frame<T>(
    stream: &mut UnixStream,
    what: &str,
    mut pred: impl FnMut(FrameKind) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    recv_until_deadline(stream, deadline, |_, frame| pred(frame))
        .await
        .unwrap_or_else(|| panic!("timed out waiting for {what}"))
}

/// Send `ATTACH` and return the `ATTACHED` snapshot.
pub(crate) async fn attach_frame(stream: &mut UnixStream, attach: &FrameKind) -> SessionSnapshot {
    send_frame(stream, attach).await;
    wait_frame(stream, "ATTACHED", |frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot),
        FrameKind::Error { code, message, .. } => panic!("ATTACH refused: {code:?} {message}"),
        _ => None,
    })
    .await
}

/// `ATTACH { CreateIfMissing(name) }` running `command` (the server default
/// when `None`), returning the `ATTACHED` snapshot.
pub(crate) async fn attach_create(
    stream: &mut UnixStream,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<String>,
) -> SessionSnapshot {
    let attach = FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::CreateIfMissing {
            name: name.to_owned(),
            command,
            cwd,
        },
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    };
    attach_frame(stream, &attach).await
}

/// `ATTACH { ByName(name) }`, returning the `ATTACHED` snapshot.
pub(crate) async fn attach(stream: &mut UnixStream, name: &str) -> SessionSnapshot {
    attach_frame(stream, &phux_server_testkit::attach_by_name(name)).await
}

/// Spawn and require a fresh `Ok`.
pub(crate) async fn spawned(stream: &mut UnixStream, request_id: u32, spawn: Spawn) -> ResourceId {
    match spawn_resource(stream, request_id, spawn).await {
        SpawnResult::Ok(id) => id,
        other => panic!("SPAWN_RESOURCE {request_id} failed: {other:?}"),
    }
}

/// `GET_STATE { Server }`. Also an in-order barrier for this connection.
pub(crate) async fn state(stream: &mut UnixStream, request_id: u32) -> SessionSnapshot {
    let get = Command::GetState {
        scope: StateScope::Server,
    };
    match command(stream, request_id, get).await {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
        other => panic!("GET_STATE failed: {other:?}"),
    }
}

/// Poll `GET_STATE` until `ready` holds.
pub(crate) async fn wait_for_state(
    stream: &mut UnixStream,
    first_request_id: u32,
    what: &str,
    ready: impl Fn(&SessionSnapshot) -> bool,
) -> SessionSnapshot {
    let end = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    for request_id in first_request_id.. {
        let snapshot = state(stream, request_id).await;
        if ready(&snapshot) {
            return snapshot;
        }
        assert!(
            tokio::time::Instant::now() < end,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    unreachable!()
}

pub(crate) fn session<'a>(snapshot: &'a SessionSnapshot, name: &str) -> Option<&'a SessionInfo> {
    snapshot.sessions.iter().find(|s| s.name == name)
}

pub(crate) fn find<'a>(
    snapshot: &'a SessionSnapshot,
    pane: &ResourceId,
) -> Option<&'a ResourceInfo> {
    snapshot.resources.iter().find(|r| &r.id == pane)
}

/// `SUBSCRIBE_EVENTS`, then a `GET_STATE` barrier proving it was installed
/// (the subscribe itself is unanswered).
pub(crate) async fn subscribe(
    stream: &mut UnixStream,
    request_id: u32,
    terminal: Option<ResourceId>,
    after_seq: Option<u64>,
) {
    send_frame(
        stream,
        &FrameKind::SubscribeEvents {
            terminal,
            after_seq,
        },
    )
    .await;
    state(stream, request_id).await;
}

/// One journaled `EVENT`.
#[derive(Debug)]
pub(crate) struct Seen {
    pub(crate) terminal: Option<ResourceId>,
    pub(crate) event: AgentEvent,
    pub(crate) stamp: Option<Box<EventStamp>>,
}

/// The next `EVENT` accepted by `pred`.
pub(crate) async fn next_event(stream: &mut UnixStream, pred: impl Fn(&Seen) -> bool) -> Seen {
    wait_frame(stream, "EVENT", |frame| match frame {
        FrameKind::Event {
            terminal,
            event,
            stamp,
        } => {
            let event = Seen {
                terminal,
                event,
                stamp,
            };
            pred(&event).then_some(event)
        }
        _ => None,
    })
    .await
}

/// Accumulate `pane`'s output (live and bootstrap replay) until it contains `needle`.
pub(crate) async fn output_containing(
    stream: &mut UnixStream,
    pane: &ResourceId,
    needle: &[u8],
) -> String {
    let mut acc = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let found = recv_until_deadline(stream, deadline, |_, frame| {
        match frame {
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } if &terminal_id == pane => acc.extend_from_slice(&bytes),
            FrameKind::BootstrapChunk {
                terminal_id,
                payload,
                ..
            } if &terminal_id == pane => acc.extend_from_slice(&payload),
            _ => return None,
        }
        acc.windows(needle.len()).any(|w| w == needle).then_some(())
    })
    .await;
    let text = String::from_utf8_lossy(&acc).into_owned();
    assert!(
        found.is_some(),
        "{pane:?} never printed {:?}; got {text:?}",
        String::from_utf8_lossy(needle)
    );
    text
}

/// `GET_METADATA`, returning the value.
pub(crate) async fn get_metadata(
    stream: &mut UnixStream,
    request_id: u32,
    scope: Scope,
    key: &str,
) -> Option<Vec<u8>> {
    let get = FrameKind::GetMetadata {
        request_id,
        scope,
        key: key.to_owned(),
    };
    send_frame(stream, &get).await;
    wait_frame(stream, "METADATA_VALUE", |frame| match frame {
        FrameKind::MetadataValue {
            request_id: got,
            value,
        } if got == request_id => Some(value),
        _ => None,
    })
    .await
}

/// A UUID-shaped request token unique per `n`.
pub(crate) fn token(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

/// Write `phux.session.create/v1` with `body`.
pub(crate) async fn send_create(
    stream: &mut UnixStream,
    request_id: u32,
    body: &serde_json::Value,
) {
    let set = FrameKind::SetMetadata {
        request_id,
        scope: Scope::Global,
        key: SESSION_CREATE_KEY.to_owned(),
        value: serde_json::to_vec(body).unwrap(),
    };
    send_frame(stream, &set).await;
}

/// Read (and so consume) the published result for `token`.
pub(crate) async fn create_result(
    stream: &mut UnixStream,
    request_id: u32,
    token: &str,
) -> Option<serde_json::Value> {
    let key = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{token}");
    get_metadata(stream, request_id, Scope::Global, &key)
        .await
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

/// Create a session from `body` (a `request_token` is added) and return its
/// result document.
pub(crate) async fn create(
    stream: &mut UnixStream,
    request_id: u32,
    mut body: serde_json::Value,
) -> serde_json::Value {
    let token = token(request_id);
    body["request_token"] = token.clone().into();
    send_create(stream, request_id, &body).await;
    create_result(stream, request_id + 10_000, &token)
        .await
        .expect("a successful create publishes its result")
}

/// Create session `name` seeded with a pane blocked on `read _`; return the pane.
pub(crate) async fn create_seeded(
    stream: &mut UnixStream,
    request_id: u32,
    name: &str,
    keep_empty: bool,
) -> ResourceId {
    let body = serde_json::json!({
        "name": name,
        "command": ["/bin/sh", "-c", "read _"],
        "keep_empty": keep_empty,
    });
    let result = create(stream, request_id, body).await;
    let id = result["terminal_id"]
        .as_u64()
        .expect("a seeded create names its pane");
    ResourceId::local(u32::try_from(id).unwrap())
}
