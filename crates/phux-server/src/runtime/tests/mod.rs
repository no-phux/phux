use super::*;
use crate::runtime::config::{
    MAX_SOCKET_PATH_LEN, ServerConfig, ServerError, validate_socket_path_len,
};
use crate::runtime::connections::socket::{socket_identity, unlink_socket_if_ours};
use crate::runtime::listeners::remote::remote_token_store;
use crate::runtime::listeners::{auto_overlay_gate_open, resolve_auto_overlay_ip};
use crate::runtime::posture::{install_policy_posture, startup_policy};
use crate::runtime::server::ServerRuntime;
use crate::runtime::startup::{mirror_config_into_state, open_org_store, resume_session_tree};
use crate::runtime::{
    attach, committed_close, handle_attach, handle_terminal_input, handle_terminal_reply,
    handle_terminal_resize, handle_viewport_resize,
};
use crate::state::{ClientId, Outbound, SharedState};
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::{AttachTarget, ErrorCode, FrameKind, ViewportInfo};
use phux_protocol::wire::{ListenerDisabledReason, RemoteListenerSlot, RemoteListenerTransport};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::runtime::Builder;
use tokio::task::{JoinSet, LocalSet};
use tokio_util::sync::CancellationToken;

/// Register one Terminal in `state` backed by a running no-PTY actor.
/// Returns its core id, its wire id, and its engine token.
fn register_seeded_pane(
    state: &mut crate::state::ServerState,
    session: phux_core::ids::SessionId,
) -> (
    phux_core::ids::ResourceId,
    phux_protocol::ids::ResourceId,
    CancellationToken,
) {
    let pane = state.add_pane_to_session(session).expect("pane");
    let bundle =
        phux_terminal::actor::TerminalActor::new_with_seed(20, 5, b"seeded").expect("actor");
    let token = bundle.token;
    tokio::task::spawn_local(bundle.actor.run());
    let wire = state.register_resource_handle(pane, bundle.handle, token.clone());
    (pane, wire, token)
}

/// An old image holding a pane retained after it exited with status 5
/// and a running pane that asked to be retained. Returns the image,
/// both panes' wire ids (exited, running), and their engine tokens.
fn old_image_with_retained_panes() -> (
    crate::state::ServerState,
    phux_protocol::ids::ResourceId,
    phux_protocol::ids::ResourceId,
    [CancellationToken; 2],
) {
    let mut old = crate::state::ServerState::new();
    let sid = old.seed_empty_session("main").expect("session");
    let (exited, exited_wire, exited_token) = register_seeded_pane(&mut old, sid);
    let (running, running_wire, running_token) = register_seeded_pane(&mut old, sid);
    let _ = old.build_session_snapshot(sid);
    old.note_retain_request(running, 60);
    old.note_retain_request(exited, 60);
    let _retention = old
        .retain_exited(exited, phux_core::process::ExitOutcome::exited(5), 1)
        .expect("retained");
    (
        old,
        exited_wire,
        running_wire,
        [exited_token, running_token],
    )
}

/// The next `RESOURCE_CLOSED` for `wire` on `rx`: its reason and exit
/// status.
async fn next_close_of(
    rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
    wire: &phux_protocol::ids::ResourceId,
) -> (phux_protocol::wire::frame::CloseReason, Option<i32>) {
    use phux_protocol::wire::frame::FrameKind;
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("RESOURCE_CLOSED arrives");
        match frame {
            Some(Outbound::Frame(FrameKind::ResourceClosed {
                terminal_id,
                reason,
                exit_status,
                ..
            })) if &terminal_id == wire => return (reason, exit_status),
            Some(_) => {}
            None => panic!("the mailbox closed before RESOURCE_CLOSED"),
        }
    }
}

fn resume_handoff_blob(
    listener_fd: std::os::fd::RawFd,
    sessions: Vec<crate::upgrade::blob::SessionBlob>,
    windows: Vec<crate::upgrade::blob::WindowBlob>,
    panes: Vec<crate::upgrade::blob::PaneBlob>,
) -> crate::upgrade::blob::StateBlob {
    crate::upgrade::blob::StateBlob {
        version: crate::upgrade::blob::BLOB_VERSION,
        listener_fd,
        counters: crate::upgrade::blob::Counters {
            next_session_wire_id: 10,
            next_terminal_wire_id: 10,
            next_window_wire_id: 10,
            next_touch_timestamp: 1,
            server_instance: None,
        },
        sessions,
        windows,
        panes,
        agent_sessions: Vec::new(),
        metadata: Vec::new(),
        org: None,
        delegations: Vec::new(),
        native_agents: Vec::new(),
    }
}

fn write_resume_fd(blob: &crate::upgrade::blob::StateBlob) -> std::os::fd::RawFd {
    use std::io::Write;
    use std::os::fd::IntoRawFd;
    let mut file = tempfile::tempfile().expect("tempfile");
    file.write_all(&blob.to_bytes().expect("serialize"))
        .expect("write blob");
    file.into_raw_fd()
}

fn bind_resume_listener(dir: &tempfile::TempDir) -> (std::path::PathBuf, std::os::fd::RawFd) {
    use std::os::fd::IntoRawFd;
    let path = dir.path().join("resume.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    (path, listener.into_raw_fd())
}

/// Bound on waits for frames already handed to the mailbox; generous so
/// a loaded machine does not produce false failures.
const MAILBOX_DEADLINE: Duration = Duration::from_secs(30);

/// Receivers for a [`stub_terminal`] handle; every channel stays open so
/// sends succeed, and tests read the ones they observe.
struct StubRx {
    snapshot: tokio::sync::mpsc::Receiver<phux_terminal::actor::SnapshotRequest>,
    resize: phux_terminal::actor::ResizeReceiver,
    encoded_input: tokio::sync::mpsc::Receiver<phux_terminal::actor::EncodedInputRequest>,
    consumer_attach: tokio::sync::mpsc::Receiver<phux_terminal::actor::ConsumerAttachRequest>,
    consumer_detach: tokio::sync::mpsc::Receiver<phux_terminal::actor::ConsumerDetachRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_bootstrap: tokio::sync::mpsc::Receiver<phux_terminal::actor::NativeBootstrapRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_publication: tokio::sync::mpsc::Receiver<phux_terminal::actor::NativePublicationRequest>,
    _keep: Vec<Box<dyn std::any::Any>>,
}

/// A Terminal resource handle with no actor behind it.
fn stub_terminal() -> (crate::resource::ResourceHandle, StubRx) {
    use tokio::sync::mpsc::channel;
    let mut keep: Vec<Box<dyn std::any::Any>> = Vec::new();
    let (input, rx) = channel(8);
    keep.push(Box::new(rx));
    let (screen, rx) = channel(8);
    keep.push(Box::new(rx));
    let (pwd, rx) = channel(8);
    keep.push(Box::new(rx));
    let (process, rx) = channel(8);
    keep.push(Box::new(rx));
    let (consumer_ack, rx) = channel(8);
    keep.push(Box::new(rx));
    let (snapshot, snapshot_rx) = channel(8);
    let (resize, resize_rx) = phux_terminal::actor::ResizeSender::channel(8);
    let (encoded_input, encoded_rx) = channel(8);
    let (consumer_attach, attach_rx) = channel(8);
    let (consumer_detach, detach_rx) = channel(8);
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let (native_bootstrap, native_bootstrap_rx) = channel(8);
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let (native_publication, native_publication_rx) = channel(8);
    let facet = phux_terminal::actor::TerminalHandle {
        consumer_attach,
        consumer_detach,
        consumer_ack,
        upgrade: channel(8).0,
        input,
        encoded_input,
        snapshot,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_bootstrap,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_publication,
        screen,
        pwd,
        process,
        resize,
        ..phux_terminal::actor::TerminalHandle::detached_for_test(80, 24)
    };
    let handle =
        crate::resource::ResourceHandle::new(None, tokio::sync::broadcast::channel(8).0, facet);
    let rx = StubRx {
        snapshot: snapshot_rx,
        resize: resize_rx,
        encoded_input: encoded_rx,
        consumer_attach: attach_rx,
        consumer_detach: detach_rx,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_bootstrap: native_bootstrap_rx,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_publication: native_publication_rx,
        _keep: keep,
    };
    (handle, rx)
}

/// Register a [`stub_terminal`] for `pane` and return its receivers. The
/// native channels are closed, so capture falls back to the VT path.
fn register_stub(state: &SharedState, pane: phux_core::ids::ResourceId) -> StubRx {
    #[cfg_attr(
        not(all(feature = "native-engine", not(target_arch = "wasm32"))),
        allow(unused_mut)
    )]
    let (handle, mut rx) = stub_terminal();
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    {
        rx.native_bootstrap.close();
        rx.native_publication.close();
    }
    state.with_mut(|s| {
        let _ = s.register_resource_handle(pane, handle, CancellationToken::new());
    });
    rx
}

/// Attach `client_id` to `session` with default caps.
fn attach_client(state: &SharedState, session: &str) -> ClientId {
    let client_id = state.with_mut(crate::state::ServerState::new_client_id);
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    state
        .with_mut(|s| s.attach_default_caps(client_id, session, tx))
        .expect("attach");
    client_id
}

/// Install a hook dispatcher whose events land on the returned receiver.
fn capture_hooks(state: &SharedState) -> tokio::sync::mpsc::Receiver<crate::hooks::HookEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    state.with_mut(|s| s.set_hook_dispatcher(crate::hooks::HookDispatcher::from_sender(tx)));
    rx
}

async fn next_frame(out_rx: &mut tokio::sync::mpsc::Receiver<Outbound>) -> FrameKind {
    match tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
        .await
        .expect("outbound frame timed out")
        .expect("outbound closed")
    {
        Outbound::Frame(frame) => frame,
        other @ Outbound::FatalError { .. } => {
            panic!("unexpected outbound sentinel: {other:?}")
        }
    }
}

const fn frame_name(frame: &FrameKind) -> &'static str {
    match frame {
        FrameKind::Attached { .. } => "ATTACHED",
        FrameKind::BootstrapBegin { .. } => "BEGIN",
        FrameKind::BootstrapChunk { .. } => "CHUNK",
        FrameKind::BootstrapReady { .. } => "READY",
        FrameKind::AttachReady { .. } => "ATTACH_READY",
        _ => "OTHER",
    }
}

async fn expect_frames(out_rx: &mut tokio::sync::mpsc::Receiver<Outbound>, expected: &[&str]) {
    for want in expected {
        let frame = next_frame(out_rx).await;
        assert_eq!(frame_name(&frame), *want, "got {frame:?}");
    }
}

/// Spawn `handle_attach` for `session` on the current `LocalSet`.
fn spawn_attach(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    session: &str,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    caps: ClientCapabilities,
    profile: phux_protocol::caps::BootstrapProfile,
) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    let out_tx = out_tx.clone();
    let target = AttachTarget::ByName(session.to_owned());
    tokio::task::spawn_local(async move {
        let token = CancellationToken::new();
        let mut output_pumps = JoinSet::new();
        handle_attach(
            &state,
            client_id,
            attach_id,
            target,
            ViewportInfo::new(80, 24),
            false,
            0,
            None,
            &out_tx,
            caps,
            profile,
            phux_protocol::caps::BootstrapLimits::default(),
            &token,
            &mut output_pumps,
            &token,
            false,
        )
        .await;
    })
}

fn state_sync_outcome(bytes: &[u8]) -> phux_terminal::actor::ConsumerAttachOutcome {
    phux_terminal::actor::ConsumerAttachOutcome {
        tick_managed: true,
        state_sync_bootstrap: Some(phux_terminal::actor::StateSyncBootstrap {
            snapshot: phux_terminal::grid::SnapshotBytes {
                cols: 80,
                rows: 24,
                bytes: bytes.to_vec(),
                scrollback: Vec::new(),
            },
            base_seq: 0,
        }),
    }
}

/// An organization whose Inbox and `bare` have no arrangement, and whose
/// `arranged` session already has `arrangement_key`.
fn org_with_one_arrangement() -> (phux_core::Org, phux_core::ArrangementKey) {
    use phux_core::{
        Arrangement, ArrangementKey, ArrangementOwner, AuthorityId, InstanceId, Org, Revision,
        SessionKey, Stamp,
    };

    let inbox_key = SessionKey::from_bytes([2; 16]);
    let arranged_key = SessionKey::from_bytes([4; 16]);
    let bare_key = SessionKey::from_bytes([6; 16]);
    let arrangement_key = ArrangementKey::from_bytes([5; 16]);
    let org = Org::new(
        AuthorityId::from_bytes([1; 16]),
        inbox_key,
        InstanceId::from_bytes([3; 16]),
        Stamp(1_000),
    );
    let inbox = org
        .sessions()
        .find(|session| session.key == inbox_key)
        .unwrap()
        .clone();
    let mut arranged = inbox.clone();
    arranged.key = arranged_key;
    arranged.name = "arranged".to_owned();
    arranged.arrangement = Some(arrangement_key);
    let mut bare = inbox.clone();
    bare.key = bare_key;
    bare.name = "bare".to_owned();
    let arrangement = Arrangement {
        key: arrangement_key,
        owner: ArrangementOwner::Session(arranged_key),
        groups: Vec::new(),
        revision: Revision::FIRST,
    };
    let org = Org::restore(
        *org.authority(),
        org.instance(),
        [inbox, arranged, bare],
        std::iter::empty::<phux_core::Resource>(),
        [arrangement],
        std::iter::empty::<phux_core::Receipt>(),
    )
    .unwrap();
    (org, arrangement_key)
}

mod attachment;
mod geometry;
mod lifecycle;
mod listeners;
mod organization;
mod upgrade;
