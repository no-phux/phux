//! The live-revocation conformance sweep (`docs/spec/workload-auth.md` §7,
//! §9 "revoked/expired authority ... while live").
//!
//! Every case runs the real client loop (`handle_client`) over a scripted
//! transport. The workload's grant is minted by the `paired` engine from a
//! real registry file (or, for expiry, by an engine whose grant lapses in
//! well under a second). Each case withdraws the grant in the middle of
//! something and checks the §7 contract: after the revocation nothing but
//! `ERROR { PERMISSION_DENIED }` and `DETACHED { reason }` reaches the
//! connection, nothing it sent or queued runs, what it held is released, and
//! its Terminals, and every other connection, carry on.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::cell::{Cell, RefCell};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use chrono::Utc;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, LayerSet};
use phux_protocol::ids::{InputOperationId, ResourceId as WireResourceId, SatelliteHost};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, ControlAction, DetachReason, ErrorCode,
    FrameKind, InputMode, Scope, ViewportInfo,
};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, LocalSet};
use tokio_util::sync::CancellationToken;

use super::client::handle_client;
use super::input_lane::{InputLaneHandle, spawn_input_lane};
use super::revocation::{Watcher, revoke_connection, spawn_revocation_watcher};
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::hub::relay::{HubRelays, RelayHandle, RelayRequest, Unsubscribe};
use crate::policy::{
    ConnectionGrant, GrantFuture, PolicyEngine, PolicyError, Revocation, ScopedPolicy,
};
use crate::state::{ClientId, EventRecord, Outbound, ServerState, SharedState};
use crate::transport::{FrameReader, FrameWriter};
use crate::workload::{ReloadingWorkloadRegistry, WorkloadRegistry};

mod stores;

/// The broken-registry grace window the fixture's watcher uses: short, so
/// the cases that wait it out stay fast.
const TEST_GRACE: Duration = Duration::from_millis(300);

/// The workload's public key, as the registry records it.
const KEY: &[u8] = b"revocation-conformance-key";

/// How long any one expectation may take before the case fails.
const PATIENCE: Duration = Duration::from_secs(5);

/// Frames the test feeds the connection whenever it chooses, then silence.
struct Feed(mpsc::UnboundedReceiver<BytesMut>);

impl FrameReader for Feed {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        match self.0.recv().await {
            Some(frame) => Ok(Some(frame)),
            None => std::future::pending().await,
        }
    }
}

/// Everything the server writes, decoded, and whether it closed.
#[derive(Clone, Default)]
struct Wire {
    frames: Rc<RefCell<Vec<FrameKind>>>,
    closed: Rc<Cell<bool>>,
}

#[allow(
    clippy::unused_async_trait_impl,
    reason = "the recording test writer implements the production async transport trait without I/O"
)]
impl FrameWriter for Wire {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        let decoded = FrameKind::decode(frame).expect("server frame").0;
        self.frames.borrow_mut().push(decoded);
        Ok(())
    }

    async fn close(&mut self) -> io::Result<()> {
        self.closed.set(true);
        Ok(())
    }
}

/// Mints a scoped grant over `set` that expires `ttl` after its HELLO.
#[derive(Debug)]
struct Expiring {
    set: TerminalScopeSet,
    ttl: chrono::Duration,
}

impl PolicyEngine for Expiring {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let grant = ConnectionGrant::scoped(self.set.clone(), credential.map(|c| c.id.clone()))
            .map(|mut grant| {
                grant.expires_at = Some(Utc::now() + self.ttl);
                grant
            })
            .map_err(|err| PolicyError::Internal(err.to_string()));
        Box::pin(async move { grant })
    }
}

/// A `paired` server with one session, `alpha`, whose pane has a live
/// (no-PTY) actor, and a workload credential in its registry.
struct Fixture {
    _dir: tempfile::TempDir,
    registry: PathBuf,
    state: SharedState,
    root: CancellationToken,
    credential_id: String,
    alpha: WireResourceId,
    alpha_core: phux_core::ids::ResourceId,
    /// The watcher the cases drive by hand, one sweep at a time.
    watcher: RefCell<Watcher>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.root.cancel();
    }
}

impl Fixture {
    /// The `paired` engine over a registry whose one record's ceiling is
    /// `scopes`.
    fn paired(scopes: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("workload-keys");
        let credential_id = WorkloadRegistry::register(&registry, KEY, strings(scopes), None)
            .unwrap()
            .id;
        let engine = Arc::new(ScopedPolicy::paired(Arc::new(
            ReloadingWorkloadRegistry::load(registry.clone()).unwrap(),
        )));
        Self::with_engine(dir, registry, credential_id, engine)
    }

    /// An engine whose grants over `*@global` lapse `ttl` after HELLO.
    fn expiring(ttl: chrono::Duration) -> Self {
        let engine = Arc::new(Expiring {
            set: TerminalScopeSet::parse_all(&["*@global"]).unwrap(),
            ttl,
        });
        let credential_id = format!("sha256:{}", "e".repeat(64));
        Self::with_engine(
            tempfile::tempdir().unwrap(),
            PathBuf::new(),
            credential_id,
            engine,
        )
    }

    fn with_engine(
        dir: tempfile::TempDir,
        registry: PathBuf,
        credential_id: String,
        engine: Arc<dyn PolicyEngine>,
    ) -> Self {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let alpha_core = super::commands::seed_session_with_actor(
            &state,
            "alpha",
            phux_config::ScrollbackLimits::default(),
            &root,
        )
        .unwrap();
        let alpha = state.with_mut(|s| {
            s.set_policy_engine(engine);
            s.intern_terminal_wire(alpha_core)
        });
        Self {
            _dir: dir,
            registry,
            state,
            root,
            credential_id,
            alpha,
            alpha_core,
            watcher: RefCell::new(Watcher::with_grace(TEST_GRACE)),
        }
    }

    /// One watcher sweep, as a poll would run it.
    fn sweep(&self) {
        let _ = self.watcher.borrow_mut().sweep(&self.state);
    }

    /// Start a connection stamped with `identity`, without its HELLO.
    fn connect(&self, identity: ConnectionIdentity, lane: Option<InputLaneHandle>) -> Connection {
        let client = self.state.with_mut(ServerState::new_client_id);
        let transport = identity.peer.transport;
        self.state
            .with_mut(|s| s.set_connection_identity(client, identity));
        let (feed, rx) = mpsc::unbounded_channel();
        let wire = Wire::default();
        let token = self.root.child_token();
        let task = tokio::task::spawn_local(handle_client(
            Feed(rx),
            wire.clone(),
            self.state.clone(),
            client,
            token.clone(),
            self.root.clone(),
            lane,
            transport,
            false,
        ));
        Connection {
            client,
            feed,
            wire,
            token,
            task,
        }
    }

    /// A workload connection past its HELLO.
    async fn workload(&self) -> Connection {
        self.workload_with_lane(None).await
    }

    async fn workload_with_lane(&self, lane: Option<InputLaneHandle>) -> Connection {
        let connection = self.connect(workload_identity(&self.credential_id), lane);
        connection.hello().await;
        connection
    }

    /// The owner's connection over its Unix socket, past its HELLO.
    async fn owner(&self) -> Connection {
        let connection = self.connect(owner_identity(), None);
        connection.hello().await;
        connection
    }

    /// `phux workload revoke` on the fixture's credential.
    fn revoke_key(&self) {
        WorkloadRegistry::revoke(&self.registry, &self.credential_id).unwrap();
    }

    /// Replace the credential's ceiling with `scopes`, as a new generation.
    fn rewrite_ceiling(&self, scopes: &[&str]) {
        rewrite_registry(&self.registry, |doc| {
            doc["credentials"][0]["scopes"] = serde_json::json!(scopes);
        });
    }

    fn lease_holder(&self) -> Option<ClientId> {
        self.state.with(|s| s.input_lease_holder(self.alpha_core))
    }
}

/// One scripted connection.
struct Connection {
    client: ClientId,
    feed: mpsc::UnboundedSender<BytesMut>,
    wire: Wire,
    token: CancellationToken,
    task: JoinHandle<io::Result<()>>,
}

impl Connection {
    fn send(&self, frame: &FrameKind) {
        let mut out = BytesMut::new();
        frame.encode(&mut out);
        let _ = self.feed.send(out);
    }

    fn frames(&self) -> Vec<FrameKind> {
        self.wire.frames.borrow().clone()
    }

    async fn hello(&self) {
        self.send(&hello());
        self.wait_for("HELLO_OK", |frame| {
            matches!(frame, FrameKind::HelloOk { .. })
        })
        .await;
    }

    async fn attach(&self, attach_id: u32) {
        self.send(&attach(attach_id));
        self.wait_for("ATTACHED", |frame| {
            matches!(frame, FrameKind::Attached { .. })
        })
        .await;
    }

    /// Send `command` and wait for its `Ok`.
    async fn command_ok(&self, request_id: u32, command: Command) {
        self.send(&FrameKind::Command {
            request_id,
            command,
        });
        self.wait_for("COMMAND_RESULT Ok", |frame| {
            matches!(
                frame,
                FrameKind::CommandResult { request_id: id, result: CommandResult::Ok } if *id == request_id
            )
        })
        .await;
    }

    async fn wait_for(&self, what: &str, found: impl Fn(&FrameKind) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !self.wire.frames.borrow().iter().any(&found) {
            assert!(
                Instant::now() < deadline,
                "no {what} within {PATIENCE:?}: {:?}",
                self.frames()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn wait_closed(&self) {
        let deadline = Instant::now() + PATIENCE;
        while !self.wire.closed.get() {
            assert!(
                Instant::now() < deadline,
                "the transport never closed: {:?}",
                self.frames()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// §7 steps 3 and 4: the transport closed, and the last two frames on
    /// it are the goodbye. Anything racing the revocation had its chance to
    /// land after it and did not.
    async fn assert_goodbye(&self, reason: DetachReason) {
        self.wait_closed().await;
        settle().await;
        let frames = self.frames();
        assert!(
            matches!(
                frames.as_slice(),
                [
                    ..,
                    FrameKind::Error {
                        request_id: None,
                        code: ErrorCode::PermissionDenied,
                        ..
                    },
                    FrameKind::Detached { reason: Some(said), .. },
                ] if *said == reason
            ),
            "expected the {reason:?} goodbye last: {frames:?}"
        );
    }

    /// The goodbye, and the connection token cancelled (step 4).
    async fn assert_ended_with(&self, reason: DetachReason) {
        self.assert_goodbye(reason).await;
        assert!(
            self.token.is_cancelled(),
            "the connection token is cancelled"
        );
    }

    fn is_open(&self) -> bool {
        !self.wire.closed.get()
            && !self
                .frames()
                .iter()
                .any(|frame| matches!(frame, FrameKind::Detached { .. }))
    }

    async fn finish(self) {
        self.token.cancel();
        let _ = self.task.await;
    }
}

/// Let every ready task run, then a little wall-clock time on top.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
}

async fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what} never held");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn strings(scopes: &[&str]) -> Vec<String> {
    scopes.iter().map(|scope| (*scope).to_owned()).collect()
}

/// Edit the registry file and commit the edit as the next generation, the
/// way an atomic writer replaces it.
fn rewrite_registry(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut doc: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    edit(&mut doc);
    let next = doc["generation"].as_u64().unwrap() + 1;
    doc["generation"] = next.into();
    write_owner_only(path, &serde_json::to_vec_pretty(&doc).unwrap());
}

/// Edit the registry file the way a hand editor would: a new file, the
/// same generation.
fn hand_edit_registry(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let raw = std::fs::read(path).unwrap();
    let mut doc: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    edit(&mut doc);
    write_owner_only(path, &serde_json::to_vec_pretty(&doc).unwrap());
}

fn write_owner_only(path: &Path, bytes: &[u8]) {
    let staged = path.with_extension("staged");
    std::fs::write(&staged, bytes).unwrap();
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(&staged, path).unwrap();
}

/// A workload's QUIC connection, its certificate verified. The cached
/// credential claims `*@global`; the grant always comes from the engine.
fn workload_identity(id: &str) -> ConnectionIdentity {
    ConnectionIdentity {
        peer: PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: Some(id.to_owned()),
            transport: TransportType::Quic,
            source_addr: None,
        },
        credential: Some(AuthenticatedCredential {
            id: id.to_owned(),
            principal: id.to_owned(),
            scopes: vec!["*@global".to_owned()],
            issued_at: Utc::now(),
            expires_at: None,
            generation: 1,
            registry_instance: None,
        }),
        ssh_origin: None,
        bearer: None,
    }
}

/// The owner's Unix socket: the serving uid, which `paired` keeps at the
/// owner's grant.
fn owner_identity() -> ConnectionIdentity {
    PeerIdentity {
        uid: nix::unistd::geteuid().as_raw(),
        pid: None,
        exe_path: None,
        mcp_host_key: None,
        transport: TransportType::UnixSocket,
        source_addr: None,
    }
    .into()
}

fn hello() -> FrameKind {
    FrameKind::Hello {
        client_name: "revocation-conformance".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::new().with_layers(LayerSet::all()),
    }
}

fn attach(attach_id: u32) -> FrameKind {
    FrameKind::Attach {
        attach_id,
        target: AttachTarget::ByName("alpha".to_owned()),
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    }
}

fn acquire(terminal_id: &WireResourceId) -> Command {
    Command::AcquireInput {
        terminal_id: terminal_id.clone(),
        mode: InputMode::Cooperative,
        ttl_ms: 0,
    }
}

fn subscribe_metadata(key: &str) -> FrameKind {
    FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: key.to_owned(),
    }
}

/// Run `case` on a current-thread runtime's `LocalSet`, as the server does.
async fn local(case: impl std::future::Future<Output = ()>) {
    LocalSet::new().run_until(case).await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoking_a_key_terminates_its_live_connections_with_authorization_revoked() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let attached = fx.workload().await;
        attached.attach(1).await;
        let idle = fx.workload().await;

        fx.revoke_key();
        fx.sweep();

        for connection in [&attached, &idle] {
            connection
                .assert_ended_with(DetachReason::AuthorizationRevoked)
                .await;
            assert!(fx.state.with(|s| s.connection_revoked(connection.client)));
        }
        assert!(
            fx.state
                .with(|s| !s.attached().contains_key(&attached.client)),
            "the attachment is released"
        );
        attached.finish().await;
        idle.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn expiry_terminates_with_authorization_expired_and_releases_leases() {
    local(async {
        let fx = Fixture::expiring(chrono::Duration::milliseconds(750));
        spawn_revocation_watcher(&fx.state, &fx.root);
        let agent = fx.workload().await;
        agent.attach(1).await;
        agent.command_ok(2, acquire(&fx.alpha)).await;
        assert_eq!(fx.lease_holder(), Some(agent.client));

        // The watcher wakes at the expiry itself: nothing else happens here.
        agent
            .assert_ended_with(DetachReason::AuthorizationExpired)
            .await;
        assert_eq!(
            fx.lease_holder(),
            None,
            "the expired grant's lease is released"
        );
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ceiling_reduction_that_no_longer_contains_a_minted_clause_revokes() {
    local(async {
        let fx = Fixture::paired(&["observe,bind,input@global"]);
        let agent = fx.workload().await;
        let generation = |client| {
            fx.state
                .with(|s| s.connection_grant(client).unwrap().registry_generation)
        };
        let minted = generation(agent.client);

        // A wider ceiling still contains every minted clause: the grant
        // stays as minted and adopts the new generation.
        fx.rewrite_ceiling(&["observe,bind,input,signal@global"]);
        fx.sweep();
        settle().await;
        assert!(
            agent.is_open(),
            "a wider ceiling ended the connection: {:?}",
            agent.frames()
        );
        assert!(
            generation(agent.client) > minted,
            "the grant adopts the new generation"
        );

        // Losing `bind` and `input` no longer contains the minted clause.
        fx.rewrite_ceiling(&["observe@global"]);
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_reload_revokes_every_workload_session_until_a_valid_generation_loads() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let first = fx.workload().await;
        let second = fx.workload().await;

        write_owner_only(&fx.registry, b"{ this is not a registry");
        fx.sweep();
        settle().await;
        assert!(
            first.is_open() && second.is_open(),
            "a broken registry is no verdict inside its grace window"
        );

        // The last known-good generation is not preserved: HELLO is refused.
        let refused = fx.connect(workload_identity(&fx.credential_id), None);
        refused.send(&hello());
        refused
            .wait_for("the HELLO refusal", |frame| {
                matches!(
                    frame,
                    FrameKind::Error {
                        code: ErrorCode::PermissionDenied,
                        ..
                    }
                )
            })
            .await;

        // The same broken state past the grace window applies the empty
        // snapshot: every workload connection it stood behind ends.
        tokio::time::sleep(TEST_GRACE + Duration::from_millis(50)).await;
        fx.sweep();
        for connection in [&first, &second] {
            connection
                .assert_ended_with(DetachReason::AuthorizationRevoked)
                .await;
        }

        // A valid generation admits the key again.
        std::fs::remove_file(&fx.registry).unwrap();
        WorkloadRegistry::register(&fx.registry, KEY, strings(&["*@global"]), None).unwrap();
        let admitted = fx.workload().await;
        assert!(admitted.is_open());

        for connection in [first, second, refused, admitted] {
            connection.finish().await;
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revocation_releases_input_leases_and_subscriptions_and_journals_them() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let observer = fx.state.with_mut(ServerState::new_client_id);
        let (observer_tx, mut observer_rx) = mpsc::channel(256);
        fx.state
            .with_mut(|s| s.subscribe_events(observer, None, observer_tx));

        let agent = fx.workload().await;
        agent.attach(1).await;
        agent.command_ok(2, acquire(&fx.alpha)).await;
        agent.send(&FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None,
        });
        agent.send(&subscribe_metadata("conformance.probe"));
        wait_until("the subscriptions are registered", || {
            fx.state.with(|s| {
                s.has_event_subscription(agent.client)
                    && s.has_metadata_subscription(
                        agent.client,
                        &Scope::Global,
                        "conformance.probe",
                    )
            })
        })
        .await;

        fx.revoke_key();
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;

        assert_eq!(fx.lease_holder(), None, "the input lease is released");
        fx.state.with(|s| {
            assert!(
                !s.has_event_subscription(agent.client),
                "event subscription dropped"
            );
            assert!(
                !s.has_metadata_subscription(agent.client, &Scope::Global, "conformance.probe"),
                "metadata subscription dropped"
            );
            assert!(
                !s.attached().contains_key(&agent.client),
                "attachment dropped"
            );
        });

        // The pane's actor journals the server's release, with no actor.
        let released = tokio::time::timeout(PATIENCE, async {
            loop {
                match observer_rx.recv().await {
                    Some(Outbound::Frame(FrameKind::Event {
                        terminal,
                        event:
                            AgentEvent::TerminalControl {
                                action: ControlAction::Released,
                                actor,
                                ..
                            },
                        ..
                    })) => return (terminal, actor),
                    Some(_) => {}
                    None => panic!("the observer's mailbox closed"),
                }
            }
        })
        .await
        .expect("the release is journaled");
        assert_eq!(released, (Some(fx.alpha.clone()), None));
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revocation_does_not_kill_the_workloads_terminals() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let agent = fx.workload().await;
        agent.attach(1).await;

        fx.revoke_key();
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;

        fx.state.with(|s| {
            assert!(
                s.resource_handle(fx.alpha_core).is_some(),
                "the Terminal survives"
            );
            assert!(
                s.find_session_by_name("alpha").is_some(),
                "its session survives"
            );
        });
        let owner = fx.owner().await;
        owner.attach(1).await;
        assert!(owner.is_open());
        agent.finish().await;
        owner.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_client_frames_after_revocation_are_not_processed() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let agent = fx.workload().await;
        agent.attach(1).await;

        // Frames the loop has not read yet, then the revocation before it
        // runs again: step 4 closes without processing them.
        agent.send(&FrameKind::Command {
            request_id: 2,
            command: acquire(&fx.alpha),
        });
        agent.send(&subscribe_metadata("queued.probe"));
        revoke_connection(&fx.state, agent.client, Revocation::Revoked);

        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        assert_eq!(
            fx.lease_holder(),
            None,
            "the queued ACQUIRE_INPUT never ran"
        );
        assert!(
            !fx.state.with(|s| s.has_metadata_subscription(
                agent.client,
                &Scope::Global,
                "queued.probe"
            )),
            "the queued SUBSCRIBE_METADATA never ran"
        );
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_revoked_grant_admits_no_frame_even_before_the_connection_closes() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let agent = fx.workload().await;

        // Step 1 alone: the grant is revoked, the token not yet cancelled.
        fx.state
            .with_mut(|s| s.mark_connection_revoked(agent.client, Revocation::Revoked));
        agent.send(&FrameKind::Command {
            request_id: 2,
            command: acquire(&fx.alpha),
        });
        agent.send(&subscribe_metadata("guarded.probe"));
        agent.send(&FrameKind::Ping { nonce: 5 });
        settle().await;

        assert_eq!(fx.lease_holder(), None, "the guard refused ACQUIRE_INPUT");
        assert!(
            !fx.state.with(|s| s.has_metadata_subscription(
                agent.client,
                &Scope::Global,
                "guarded.probe"
            )),
            "the guard refused SUBSCRIBE_METADATA"
        );
        agent
            .assert_goodbye(DetachReason::AuthorizationRevoked)
            .await;
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_connection_revoked_before_hello_closes_without_a_frame() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let pending = fx.connect(workload_identity(&fx.credential_id), None);
        settle().await;

        revoke_connection(&fx.state, pending.client, Revocation::Revoked);
        pending.send(&hello());
        pending.wait_closed().await;
        settle().await;
        assert!(
            pending.frames().is_empty(),
            "no HELLO_OK and no DETACHED before a handshake: {:?}",
            pending.frames()
        );
        pending.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_humans_session_survives_the_agents_revocation() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let human = fx.owner().await;
        human.attach(1).await;
        let agent = fx.workload().await;
        agent.attach(1).await;
        agent.command_ok(2, acquire(&fx.alpha)).await;

        fx.revoke_key();
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;

        // The human keeps its attachment and can take the wheel the agent
        // lost; nothing about its connection changed.
        human.command_ok(3, acquire(&fx.alpha)).await;
        human.send(&FrameKind::Ping { nonce: 11 });
        human
            .wait_for("PONG", |frame| {
                matches!(frame, FrameKind::Pong { nonce: 11 })
            })
            .await;
        assert!(human.is_open(), "{:?}", human.frames());
        assert_eq!(fx.lease_holder(), Some(human.client));
        fx.state.with(|s| {
            assert!(s.attached().contains_key(&human.client));
            assert!(!s.connection_revoked(human.client));
        });
        agent.finish().await;
        human.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoking_mid_subscribe_stops_an_in_flight_replay() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        fx.state.with_mut(|s| {
            for _ in 0..512 {
                let _ =
                    s.record_and_fanout(EventRecord::new(Some(fx.alpha.clone()), AgentEvent::Bell));
            }
        });
        let agent = fx.workload().await;
        agent.send(&FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: Some(0),
        });
        agent
            .wait_for("a replayed EVENT", |frame| {
                matches!(frame, FrameKind::Event { .. })
            })
            .await;

        fx.revoke_key();
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        assert!(
            !fx.state.with(|s| s.has_event_subscription(agent.client)),
            "the replaying subscription is dropped"
        );
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoking_mid_input_drops_the_pending_apply_input_receipt() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let lane = spawn_input_lane(fx.state.clone()).unwrap();
        let agent = fx.workload_with_lane(Some(lane.handle())).await;
        agent.attach(1).await;

        agent.send(&FrameKind::Command {
            request_id: 4,
            command: Command::ApplyInput {
                operation_id: InputOperationId::new([7; 16]).unwrap(),
                terminal_id: fx.alpha.clone(),
                events: vec![InputEvent::Focus(FocusEvent::Gained)],
            },
        });
        settle().await;
        revoke_connection(&fx.state, agent.client, Revocation::Revoked);

        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        agent.finish().await;
        drop(lane);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoking_a_hub_consumer_tears_down_its_relay_state() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let host = SatelliteHost::new("sat");
        let (relay, mut mailbox) = RelayHandle::new(host.clone());
        let relays = HubRelays::default();
        relays.insert(relay);
        fx.state.with_mut(|s| s.set_hub_relays(relays));

        let agent = fx.workload().await;
        let (lease_tx, _lease_rx) = mpsc::channel(1);
        let proxies_cancel = fx.state.with_mut(|s| {
            let _ = s.set_satellite_lease(host.clone(), 7, agent.client, lease_tx);
            s.client_connection_cancellation(agent.client).unwrap()
        });

        revoke_connection(&fx.state, agent.client, Revocation::Revoked);
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;

        assert!(
            proxies_cancel.is_cancelled(),
            "every relay proxy keyed on the connection token is torn down"
        );
        assert!(
            matches!(mailbox.unsubscribes.try_recv(), Ok(Unsubscribe::Client(client)) if client == agent.client),
            "the hub drops the consumer's proxy subscriptions"
        );
        assert!(
            matches!(
                mailbox.requests.try_recv(),
                Ok(RelayRequest::Command { command: Command::ReleaseInput { terminal_id }, .. })
                    if terminal_id == WireResourceId::local(7)
            ),
            "the satellite lease is released over the link"
        );
        assert!(fx.state.with(|s| s.satellite_leases_held_by(agent.client).is_empty()));
        agent.finish().await;
    })
    .await;
}
