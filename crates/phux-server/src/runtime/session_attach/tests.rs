//! Actual HELLO/ATTACH dispatch with all four resource engines.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use bytes::{Bytes, BytesMut};
use phux_core::{Actor, LaunchSpec, SessionLifetime};
use phux_protocol::caps::{BootstrapCapabilities, BootstrapStreamProfile};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_tree::{
    DocumentActor, DocumentKind, DocumentResource as _, SurfaceDocument, TreeDocument,
};
use std::io;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

use super::*;
use crate::resource::{ResourceHandle, ResourceOutput};
use crate::transport::{FrameReader, FrameWriter};

mod failure;

struct Reader(mpsc::Receiver<BytesMut>);
impl FrameReader for Reader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        Ok(self.0.recv().await)
    }
}
struct Writer(mpsc::Sender<FrameKind>);
impl FrameWriter for Writer {
    #[allow(clippy::unused_async_trait_impl, reason = "in-memory transport close")]
    async fn close(&mut self) -> io::Result<()> {
        Ok(())
    }

    async fn write_frame(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0
            .send(FrameKind::decode(bytes).expect("wire frame").0)
            .await
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }
}

struct Connection {
    client: ClientId,
    tx: mpsc::Sender<BytesMut>,
    rx: mpsc::Receiver<FrameKind>,
    token: CancellationToken,
    task: tokio::task::JoinHandle<io::Result<()>>,
}

impl Connection {
    async fn start(state: &SharedState, terminal_profile: bool) -> Self {
        let client = state.with_mut(|s| {
            let client = s.new_client_id();
            s.set_peer_identity(
                client,
                PeerIdentity {
                    uid: nix::unistd::geteuid().as_raw(),
                    pid: None,
                    exe_path: None,
                    mcp_host_key: None,
                    transport: TransportType::UnixSocket,
                    source_addr: None,
                },
            );
            client
        });
        let (tx, input) = mpsc::channel(64);
        let (output, rx) = mpsc::channel(64);
        let token = CancellationToken::new();
        let task = tokio::task::spawn_local(crate::runtime::connections::dispatch::handle_client(
            Reader(input),
            Writer(output),
            state.clone(),
            client,
            token.clone(),
            CancellationToken::new(),
            None,
            TransportType::UnixSocket,
            false,
        ));
        let mut connection = Self {
            client,
            tx,
            rx,
            token,
            task,
        };
        let bootstrap = if terminal_profile {
            BootstrapCapabilities::default()
        } else {
            BootstrapCapabilities::new()
                .with_profiles(phux_protocol::caps::BootstrapProfileSet::new())
        };
        connection
            .send(FrameKind::Hello {
                client_name: "session-kinds-test".to_owned(),
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                client_caps: ClientCapabilities::default().with_bootstrap(bootstrap),
            })
            .await;
        let FrameKind::HelloOk {
            selected_profile, ..
        } = connection.next().await
        else {
            panic!("HELLO_OK")
        };
        assert_eq!(selected_profile.is_some(), terminal_profile);
        connection
    }

    async fn send(&self, frame: FrameKind) {
        let mut bytes = BytesMut::new();
        frame.encode(&mut bytes);
        self.tx.send(bytes).await.unwrap();
    }

    async fn next(&mut self) -> FrameKind {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.rx.recv())
            .await
            .expect("dispatch deadline")
            .expect("connection closed")
    }

    async fn attach(&self, id: u32) {
        self.send(FrameKind::Attach {
            attach_id: id,
            target: AttachTarget::ByName("target".to_owned()),
            viewport: ViewportInfo::new(123, 45),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: Some(RolePolicy::TAKEOVER),
        })
        .await;
    }

    async fn finish(self) {
        self.token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), self.task)
            .await
            .expect("connection teardown deadline")
            .unwrap()
            .unwrap();
    }
}

fn parent(
    state: &SharedState,
    terminal: bool,
) -> (phux_core::ids::SessionId, phux_core::ids::ResourceId) {
    if terminal {
        let id = crate::runtime::commands::seed_session_with_actor(
            state,
            "target",
            phux_config::ScrollbackLimits::default(),
            &CancellationToken::new(),
        )
        .unwrap();
        return (state.with(|s| s.registry().session_of(id).unwrap()), id);
    }
    state.with_mut(|s| {
        let (_, parent) = s.seed_session("producer");
        let target = s
            .create_empty_session("target", SessionLifetime::Persistent, &Actor::owner())
            .unwrap();
        (target, parent)
    })
}

fn document<K: DocumentKind>(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    session: phux_core::ids::SessionId,
    launch: LaunchSpec,
) -> (phux_core::ids::ResourceId, ResourceHandle) {
    let core = state.with_mut(|s| {
        let core = s.create_document(parent, launch, &Actor::owner()).unwrap();
        s.move_resource_to(core, session, &Actor::owner()).unwrap();
        core
    });
    let token = CancellationToken::new();
    let bundle = DocumentActor::<K>::build(parent, "fixture", token.clone());
    let handle = bundle.handle;
    state.with_mut(|s| {
        s.register_resource_handle(core, handle.clone(), token).unwrap();
    });
    tokio::task::spawn_local(bundle.actor.run());
    (core, handle)
}

fn tree(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    session: phux_core::ids::SessionId,
) -> (phux_core::ids::ResourceId, ResourceHandle) {
    document::<TreeDocument>(
        state,
        parent,
        session,
        LaunchSpec::Tree(phux_core::resource::TreeLaunch {
            target: "fixture".to_owned(),
        }),
    )
}

fn agent_stub(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    session: phux_core::ids::SessionId,
) -> (
    phux_core::ids::ResourceId,
    mpsc::Receiver<phux_agent_session::session::BootstrapRequest>,
) {
    let core = state.with_mut(|s| {
        let core = s
            .new_agent_session(
                parent,
                &phux_core::resource::AgentFacet {
                    provider: "fixture".to_owned(),
                    native_id: None,
                    state: None,
                },
            )
            .unwrap();
        s.move_resource_to(core, session, &Actor::owner()).unwrap();
        core
    });
    let (bootstrap, rx) = mpsc::channel(4);
    let (append, _append_rx) = mpsc::channel(1);
    let (output, _) = tokio::sync::broadcast::channel(8);
    let handle = ResourceHandle::new(
        Some(parent),
        output,
        phux_agent_session::session::AgentSessionHandle {
            provider: "fixture".into(),
            native_id: None,
            append,
            bootstrap,
            ended: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );
    state.with_mut(|s| {
        s.register_resource_handle(core, handle, CancellationToken::new()).unwrap();
    });
    (core, rx)
}

fn agent_cut(records: Vec<Bytes>) -> phux_agent_session::session::AgentSessionBootstrap {
    phux_agent_session::session::AgentSessionBootstrap {
        base_seq: 0,
        resume_after_seq: None,
        records,
        dropped: 0,
        ended: false,
        native_publications: Vec::new(),
        native_publication_high_water: 0,
    }
}

#[test]
fn aggregate_generation_is_not_reused_by_document_lag_recovery() {
    let mut state = crate::state::ServerState::new();
    let (_, resource) = state.seed_session("generation");
    let client = state.new_client_id();
    let (_, _, seq, _) =
        state.replace_attach_terminal_pump(client, resource, initial_bootstrap_id());
    assert_eq!(
        state
            .advance_attach_terminal_pump(client, resource, &seq)
            .unwrap()
            .get(),
        2
    );
}

async fn publish<K: DocumentKind>(handle: &ResourceHandle, bytes: Bytes) {
    let (reply, rx) = oneshot::channel();
    handle
        .document::<K>()
        .unwrap()
        .publish
        .send(phux_tree::PublishRequest { bytes, reply })
        .await
        .unwrap();
    rx.await.unwrap().unwrap();
}

/// Assert the complete aggregate prefix, including the common generation,
/// and reject even one live frame before the final barrier.
async fn prefix(
    connection: &mut Connection,
    count: usize,
    attach_id: u32,
) -> Vec<BootstrapStreamProfile> {
    let FrameKind::Attached { snapshot, .. } = connection.next().await else {
        panic!("ATTACHED")
    };
    assert_eq!(
        snapshot
            .resources
            .iter()
            .filter(|r| r.session == Some(snapshot.focused_session))
            .count(),
        count
    );
    assert_ne!(
        snapshot.focused_resource,
        phux_protocol::ids::ResourceId::local(0)
    );
    let mut profiles = Vec::new();
    let mut ready = 0;
    let mut generation = None;
    loop {
        match connection.next().await {
            FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                profile,
                ..
            } => {
                let generation = generation.get_or_insert((stream_id, bootstrap_id));
                assert_eq!(*generation, (stream_id, bootstrap_id));
                profiles.push(profile);
            }
            FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                ..
            } => assert_eq!(generation, Some((stream_id, bootstrap_id))),
            FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            } => {
                assert_eq!(generation, Some((stream_id, bootstrap_id)));
                ready += 1;
            }
            FrameKind::AttachReady { attach_id: id } => {
                assert_eq!(id, attach_id);
                break;
            }
            frame => panic!("interleaved aggregate frame: {frame:?}"),
        }
    }
    assert_eq!(ready, count);
    assert_eq!(profiles.len(), count);
    profiles
}

#[tokio::test(flavor = "current_thread")]
async fn document_only_without_terminal_profile_and_reattach_are_live() {
    LocalSet::new().run_until(async {
        let state = SharedState::new();
        let (session, parent) = parent(&state, false);
        state.with_mut(|s| s.set_window_size(phux_config::WindowSize::Smallest));
        let (core, handle) = tree(&state, parent, session);
        publish::<TreeDocument>(&handle, Bytes::from_static(b"{\"cut\":1}")).await;
        let wire = state.with_mut(|s| s.intern_resource_wire(core));
        let mut connection = Connection::start(&state, false).await;
        for attach_id in [1, 2] {
            connection.attach(attach_id).await;
            assert_eq!(prefix(&mut connection, 1, attach_id).await,
                [BootstrapStreamProfile::AccessibilityTreeJsonV1]);
            assert!(state.with(|s| s.input_lease_holder(core).is_none()));
            assert!(state.with(|s| s.viewport_terminals(connection.client).is_empty()));
            assert!(state.with(|s| s.resolve_terminal_geometry(core, Some(ViewportInfo::new(123, 45))).is_none()));
            assert!(state.with(|s| s.resolve_terminal_cell_px(core).is_none()));
            state.with_mut(|s| {
                s.subscribe_resource(connection.client, parent, None);
                assert!(s.resolve_terminal_geometry(parent, None).is_none(),
                    "a document session's viewport must not size a foreign Terminal subscription");
            });
            publish::<TreeDocument>(&handle, Bytes::from_static(b"{\"live\":true}")).await;
            assert!(matches!(connection.next().await, FrameKind::ResourceOutput { terminal_id, .. } if terminal_id == wire));
        }
        assert!(state.with(|s| s.attached()[&connection.client].bootstrap_profile.is_none()));
        connection.finish().await;
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn mixed_four_kinds_share_barrier_and_only_terminal_gets_a_lease() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, true);
            let (tree, handle) = tree(&state, parent, session);
            publish::<TreeDocument>(&handle, Bytes::from_static(b"{}")).await;
            let (surface, surface_handle) = document::<SurfaceDocument>(
                &state,
                parent,
                session,
                LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
                    target: "pixels".to_owned(),
                }),
            );
            let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
            png.extend_from_slice(&1_u32.to_be_bytes());
            png.extend_from_slice(&1_u32.to_be_bytes());
            publish::<SurfaceDocument>(&surface_handle, Bytes::from(png)).await;
            state.with_mut(|s| {
                assert!(s.registry_mut().set_recent(session, Some(tree)));
                assert_eq!(s.active_resource_of_session(session), Some(tree));
                assert_eq!(s.active_pane_of_session(session), Some(parent));
                let wire = s.intern_resource_wire(tree);
                assert_eq!(
                    s.build_session_snapshot(session).unwrap().focused_resource,
                    wire
                );
            });
            let agent = state.with_mut(|s| {
                s.new_agent_session(
                    parent,
                    &phux_core::resource::AgentFacet {
                        provider: "test".to_owned(),
                        native_id: None,
                        state: None,
                    },
                )
                .unwrap()
            });
            let token = CancellationToken::new();
            let bundle = phux_agent_session::session::AgentSessionActor::build(
                parent,
                "test",
                None,
                token.clone(),
                4096,
            );
            state.with_mut(|s| {
                s.register_resource_handle(agent, bundle.handle.clone(), token).unwrap();
            });
            tokio::task::spawn_local(bundle.actor.run());
            let mut connection = Connection::start(&state, true).await;
            connection.attach(1).await;
            let profiles = prefix(&mut connection, 4, 1).await;
            assert!(profiles.contains(&BootstrapStreamProfile::AgentEventsJsonlV1));
            assert!(profiles.contains(&BootstrapStreamProfile::PixelSurfacePngV1));
            for core in [agent, tree, surface] {
                assert!(state.with(|s| s.input_lease_holder(core).is_none()));
            }
            assert_eq!(
                state.with(|s| s.viewport_terminals(connection.client)),
                [parent]
            );
            assert_eq!(
                state.with(|s| s.input_lease_holder(parent)),
                Some(connection.client)
            );
            connection.finish().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn mixed_without_terminal_profile_refuses_before_subscription_or_geometry() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, true);
            let (tree, _) = tree(&state, parent, session);
            let before = state.with(|s| s.registry().terminal(parent).unwrap().dims);
            let mut connection = Connection::start(&state, false).await;
            connection.attach(1).await;
            assert!(matches!(
                connection.next().await,
                FrameKind::Error {
                    code: ErrorCode::CodecUnavailable,
                    ..
                }
            ));
            state.with(|s| {
                assert!(!s.attached().contains_key(&connection.client));
                assert!(
                    !s.subscribers_for_resource(tree)
                        .contains(&connection.client)
                );
                assert_eq!(s.registry().terminal(parent).unwrap().dims, before);
                assert!(s.input_lease_holder(parent).is_none());
            });
            connection.send(FrameKind::Ping { nonce: 9 }).await;
            assert!(matches!(
                connection.next().await,
                FrameKind::Pong { nonce: 9 }
            ));
            connection.finish().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn captured_document_output_waits_for_every_resource_ready() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, false);
            let (_, handle) = tree(&state, parent, session);
            publish::<TreeDocument>(&handle, Bytes::from_static(b"{}")).await;
            let (_, mut cut) = agent_stub(&state, parent, session);
            let mut connection = Connection::start(&state, false).await;
            connection.attach(1).await;
            let last_capture = tokio::time::timeout(std::time::Duration::from_secs(5), cut.recv())
                .await
                .unwrap()
                .unwrap();
            handle
                .output
                .send(ResourceOutput::Live {
                    seq: 2,
                    bytes: Bytes::from_static(b"{\"during_capture\":true}"),
                    at: std::time::Instant::now(),
                })
                .unwrap();
            tokio::task::yield_now().await;
            assert!(
                connection.rx.try_recv().is_err(),
                "no partial publication or document-generation leak"
            );
            last_capture.reply.send(agent_cut(Vec::new())).unwrap();
            prefix(&mut connection, 2, 1).await;
            assert!(matches!(
                connection.next().await,
                FrameKind::ResourceOutput { seq: 2, .. }
            ));
            connection.finish().await;
        })
        .await;
}
