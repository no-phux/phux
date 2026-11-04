//! The live-revocation conformance sweep (`docs/remote.md#managing-credentials`,
//! "revoked/expired authority ... while live").
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

use std::cell::RefCell;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use chrono::Utc;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::policy::PeerIdentity;
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, DetachReason, ErrorCode, FrameKind, InputMode, Scope,
    ViewportInfo,
};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, LocalSet};
use tokio_util::sync::CancellationToken;

use super::revocation::Watcher;
use super::test_support::{Feed, Wire, hello, owner_peer, workload_identity};
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::policy::{ConnectionGrant, GrantFuture, PolicyEngine, PolicyError, ScopedPolicy};
use crate::runtime::apps::terminal::input_lane::InputLaneHandle;
use crate::runtime::connections::handle_client;
use crate::state::{ClientId, ServerState, SharedState};
use crate::workload::{ReloadingWorkloadRegistry, WorkloadRegistry};

mod in_flight;
mod stores;
mod withdrawal;

/// The broken-registry grace window the fixture's watcher uses: short, so
/// the cases that wait it out stay fast.
const TEST_GRACE: Duration = Duration::from_millis(300);

/// The workload's public key, as the registry records it.
const KEY: &[u8] = b"revocation-conformance-key";

/// How long any one expectation may take before the case fails.
const PATIENCE: Duration = Duration::from_secs(5);

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
            s.intern_resource_wire(alpha_core)
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
            Feed::silent_on_close(rx),
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
        let connection = self.connect(workload_identity(&self.credential_id, &["*@global"]), lane);
        connection.hello().await;
        connection
    }

    /// The owner's connection over its Unix socket, past its HELLO.
    async fn owner(&self) -> Connection {
        let connection = self.connect(owner_peer().into(), None);
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
        self.send(&hello("revocation-conformance"));
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
