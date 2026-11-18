//! The live-revocation conformance sweep (`docs/remote.md#managing-credentials`).
//! Each case withdraws the grant mid-operation and checks §7: only
//! `ERROR { PERMISSION_DENIED }` and `DETACHED { reason }` follow, nothing
//! queued runs, holdings are released, and other connections carry on.

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{Command, DetachReason, ErrorCode, FrameKind, InputMode, Scope};
use tokio_util::sync::CancellationToken;

use super::revocation::Watcher;
use super::test_support::{
    Connection, PATIENCE, alpha_world, hello, local, owner_peer, spawn_connection, wait_until,
    workload_identity,
};
use crate::auth::ConnectionIdentity;
use crate::policy::testing::{FnPolicy, parse_scopes, scoped_grant};
use crate::policy::{PolicyEngine, ScopedPolicy};
use crate::state::{ClientId, SharedState};
use crate::workload::{ReloadingWorkloadRegistry, WorkloadRegistry};

mod in_flight;
mod stores;
mod withdrawal;

/// The broken-registry grace window the fixture's watcher uses: short, so
/// the cases that wait it out stay fast.
const TEST_GRACE: Duration = Duration::from_millis(300);

/// The workload's public key, as the registry records it.
const KEY: &[u8] = b"revocation-conformance-key";

/// Mints a scoped grant over `set` that expires `ttl` after its HELLO.
fn expiring(set: TerminalScopeSet, ttl: jiff::SignedDuration) -> FnPolicy {
    FnPolicy::new(move |_, credential| {
        let mut grant = scoped_grant(&set, credential)?;
        grant.expires_at = Some(Timestamp::now() + ttl);
        Ok(grant)
    })
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
    fn expiring(ttl: jiff::SignedDuration) -> Self {
        let engine = Arc::new(expiring(parse_scopes(&["*@global"]), ttl));
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
        let root = CancellationToken::new();
        let (state, alpha, alpha_core) = alpha_world(engine, &root);
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
    fn connect(&self, identity: ConnectionIdentity) -> Connection {
        let client = self.state.with_mut(|s| s.clients_mut().new_client_id());
        let transport = identity.peer.transport;
        self.state
            .with_mut(|s| s.clients_mut().set_connection_identity(client, identity));
        spawn_connection(&self.state, client, transport, &self.root, None, false)
    }

    /// A workload connection past its HELLO.
    async fn workload(&self) -> Connection {
        let connection = self.connect(workload_identity(&self.credential_id, &["*@global"]));
        connection.hello().await;
        connection
    }

    /// The owner's connection over its Unix socket, past its HELLO.
    async fn owner(&self) -> Connection {
        let connection = self.connect(owner_peer().into());
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
        self.state.with(|s| s.leases().holder(self.alpha_core))
    }
}

impl Connection {
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
}

/// Let every ready task run, then a little wall-clock time on top.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
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

fn acquire(terminal_id: &WireResourceId) -> Command {
    super::test_support::acquire(terminal_id, InputMode::Cooperative)
}

fn subscribe_metadata(key: &str) -> FrameKind {
    FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: key.to_owned(),
    }
}
