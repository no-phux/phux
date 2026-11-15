//! Helpers shared by the lifecycle suites.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::time::Duration;

use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, EventStamp, FrameKind, SESSION_CREATE_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, Scope, SpawnResource, SpawnResult, ViewportInfo,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};
use phux_server::{LaunchCommand, ServerConfig};
use phux_server_testkit::{
    Spawn, WIRE_RECV_TIMEOUT, command, recv_until_deadline, seed_pty, send_frame, spawn_resource,
};
use tokio::net::UnixStream;

pub(crate) use phux_server_testkit::SpawnedServer as Server;

/// `/bin/sh -c script` as an argv.
pub(crate) fn sh(script: &str) -> Spawn {
    Spawn::command(&["/bin/sh", "-c", script])
}

pub(crate) const FOREVER: &str = "while :; do sleep 3600; done";

/// A server seeding `demo` with a pane that never exits, `owner` attached to it.
pub(crate) async fn demo(configure: impl FnOnce(&mut ServerConfig)) -> (Server, UnixStream) {
    let server = Server::start(Some("demo"), |cfg| {
        let mut seed = LaunchCommand::new("/bin/sh");
        seed.args(["-c", FOREVER]);
        seed_pty(cfg, seed);
        configure(cfg);
    });
    let mut owner = server.connect().await;
    attach(&mut owner, "demo").await;
    (server, owner)
}

/// [`demo`] plus a never-exiting parent Terminal (request 1) to bind under.
pub(crate) async fn demo_parent() -> (Server, UnixStream, ResourceId) {
    let (server, mut owner) = demo(|_| {}).await;
    let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
    (server, owner, parent)
}

/// Spawn a non-Terminal `resource`, requiring `Ok`.
pub(crate) async fn spawn_kind(
    stream: &mut UnixStream,
    request_id: u32,
    resource: SpawnResource,
) -> ResourceId {
    let spawn = Spawn {
        resource: Some(Box::new(resource)),
        ..Spawn::default()
    };
    spawned(stream, request_id, spawn).await
}

/// `PUBLISH_RESOURCE_STATE` of `bytes` to `id`.
pub(crate) async fn publish(
    stream: &mut UnixStream,
    request_id: u32,
    id: &ResourceId,
    bytes: impl Into<Vec<u8>>,
) -> CommandResult {
    let publish = Command::PublishResourceState {
        terminal_id: id.clone(),
        bytes: bytes.into(),
    };
    command(stream, request_id, publish).await
}

pub(crate) use phux_server_testkit::send_command;

/// A bootstrap of `id`: (`BEGIN` grid, profile, `base_seq`, concatenated chunks).
pub(crate) async fn bootstrap(
    stream: &mut UnixStream,
    id: &ResourceId,
) -> ((u16, u16), BootstrapStreamProfile, u64, Vec<u8>) {
    let mut begin = None;
    let mut chunks = Vec::new();
    let mut generation = None;
    wait_frame(stream, "bootstrap", |frame| match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            cols,
            rows,
            profile,
            base_seq,
            bootstrap_id,
            ..
        } if &terminal_id == id => {
            begin = Some(((cols, rows), profile, base_seq));
            generation = Some(bootstrap_id);
            None
        }
        FrameKind::BootstrapChunk {
            terminal_id,
            payload,
            bootstrap_id,
            ..
        } if &terminal_id == id => {
            assert_eq!(
                Some(bootstrap_id),
                generation,
                "chunk from an old generation"
            );
            chunks.extend_from_slice(&payload);
            None
        }
        FrameKind::BootstrapReady {
            terminal_id,
            bootstrap_id,
            ..
        } if &terminal_id == id => {
            assert_eq!(
                Some(bootstrap_id),
                generation,
                "READY from an old generation"
            );
            Some(())
        }
        FrameKind::ResourceOutput { terminal_id, .. } if &terminal_id == id && begin.is_some() => {
            panic!("old-generation live output interleaved with the bootstrap");
        }
        _ => None,
    })
    .await;
    let (grid, profile, base_seq) = begin.expect("BOOTSTRAP_BEGIN");
    (grid, profile, base_seq, chunks)
}

/// Wait for `id`'s `BOOTSTRAP_READY`.
pub(crate) async fn bootstrap_ready(stream: &mut UnixStream, id: &ResourceId) {
    wait_frame(stream, "BOOTSTRAP_READY", |frame| match frame {
        FrameKind::BootstrapReady { terminal_id, .. } if &terminal_id == id => Some(()),
        _ => None,
    })
    .await;
}

/// The next live `RESOURCE_OUTPUT` for `id`: its seq and bytes.
pub(crate) async fn next_output(stream: &mut UnixStream, id: &ResourceId) -> (u64, Vec<u8>) {
    wait_frame(stream, "RESOURCE_OUTPUT", |frame| match frame {
        FrameKind::ResourceOutput {
            terminal_id,
            seq,
            bytes,
            ..
        } if &terminal_id == id => Some((seq, bytes.to_vec())),
        _ => None,
    })
    .await
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
    let key = phux_server_testkit::input_key(pane.clone(), enter_key());
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

pub(crate) use phux_server_testkit::server_state as state;

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
    let get = phux_server_testkit::get_metadata(request_id, scope, key);
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
    let set = phux_server_testkit::set_metadata(
        request_id,
        Scope::Global,
        SESSION_CREATE_KEY,
        serde_json::to_vec(body).unwrap(),
    );
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
