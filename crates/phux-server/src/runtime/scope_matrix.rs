//! The dispatch guard as a paired client meets it on the wire
//! (`docs/remote.md#who-can-do-what`).
//!
//! Each test runs the real client loop (`handle_client`) over a scripted
//! transport, with a workload identity stamped the way the QUIC acceptor
//! stamps it. The credential the transport cached always claims `*@global`;
//! every test passes only if the grant came from the engine instead.
//!
//! Two engines mint the grant. The `paired` [`ScopedPolicy`] reads a real
//! registry file, which may not hold `group:` or `terminal:` selectors
//! (§5.1: session and Terminal ids restart with the server). Group and
//! Terminal grants, which a future HELLO-requested attenuation carries, are
//! minted by a fixed engine so the guard's handling of them stays covered.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::ids::{GroupId, ResourceId as WireResourceId};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, Scope, SpawnResult, WHOAMI_KEY,
};
use tokio_util::sync::CancellationToken;

use super::client::handle_client;
use super::input_lane::InputLaneHandle;
use super::test_support::{Feed, Wire, encode, workload_identity};
use crate::auth::AuthenticatedCredential;
use crate::policy::{ConnectionGrant, GrantFuture, PolicyEngine, PolicyError, ScopedPolicy};
use crate::state::{ClientId, SharedState};
use crate::workload::{ReloadingWorkloadRegistry, WorkloadRegistry};

mod guard;
mod inventory;

const CLIENT: ClientId = ClientId(21);

/// Mints the same scoped grant for every connection: the shape a
/// HELLO-requested attenuation would produce, including Group and Terminal
/// selectors no registry record may hold.
#[derive(Debug)]
struct FixedGrant(TerminalScopeSet);

impl PolicyEngine for FixedGrant {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let grant = ConnectionGrant::scoped(self.0.clone(), credential.map(|c| c.id.clone()))
            .map_err(|err| PolicyError::Internal(err.to_string()));
        Box::pin(async move { grant })
    }
}

/// The ids a test's scopes may need to name.
struct Topology {
    alpha: u32,
    alpha_group: u32,
}

/// Which engine mints the grant.
#[derive(Clone, Copy)]
enum Engine {
    /// The `paired` engine over a real registry file.
    Registry,
    /// [`FixedGrant`].
    Fixed,
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: SharedState,
    credential_id: String,
    alpha: WireResourceId,
    beta: WireResourceId,
}

fn paired(scopes: &[&str]) -> Fixture {
    let scopes: Vec<String> = scopes.iter().map(|scope| (*scope).to_owned()).collect();
    fixture(Engine::Registry, |_| scopes)
}

/// Two sessions with one Terminal each; an engine whose grant is the scopes
/// `ceiling` names; and `CLIENT` stamped as a workload's QUIC connection.
fn fixture(engine: Engine, ceiling: impl FnOnce(&Topology) -> Vec<String>) -> Fixture {
    let state = SharedState::new();
    let (alpha, beta, alpha_group) = state.with_mut(|s| {
        let (alpha_session, alpha_core) = s.seed_session("alpha");
        let (_, beta_core) = s.seed_session("beta");
        let alpha = s.intern_terminal_wire(alpha_core);
        let beta = s.intern_terminal_wire(beta_core);
        (alpha, beta, s.idspace.intern_session(alpha_session).get())
    });
    let WireResourceId::Local { id: alpha_id } = alpha else {
        unreachable!("a seeded pane is local");
    };
    let scopes = ceiling(&Topology {
        alpha: alpha_id,
        alpha_group,
    });
    let dir = tempfile::tempdir().unwrap();
    let (id, policy): (String, Arc<dyn PolicyEngine>) = match engine {
        Engine::Registry => {
            let path = dir.path().join("workload-keys");
            let id = WorkloadRegistry::register(&path, b"scope-matrix-key", scopes, None)
                .unwrap()
                .id;
            let registry = Arc::new(ReloadingWorkloadRegistry::load(path).unwrap());
            (id, Arc::new(ScopedPolicy::paired(registry)))
        }
        Engine::Fixed => {
            let set = TerminalScopeSet::parse_all(&scopes).unwrap();
            let id = format!("sha256:{}", "f".repeat(64));
            (id, Arc::new(FixedGrant(set)))
        }
    };
    state.with_mut(|s| {
        s.set_policy_engine(policy);
        s.set_connection_identity(CLIENT, workload_identity(&id, &["*@global"]));
    });
    Fixture {
        _dir: dir,
        state,
        credential_id: id,
        alpha,
        beta,
    }
}

fn script(frames: &[FrameKind]) -> Vec<BytesMut> {
    frames.iter().map(encode).collect()
}

fn command(request_id: u32, command: Command) -> FrameKind {
    FrameKind::Command {
        request_id,
        command,
    }
}

fn get_screen(request_id: u32, terminal_id: WireResourceId) -> FrameKind {
    command(
        request_id,
        Command::GetScreen {
            terminal_id,
            request_scrollback: None,
            cells: false,
            format: 0,
        },
    )
}

fn paste(terminal_id: &WireResourceId, data: Vec<u8>) -> FrameKind {
    FrameKind::InputPaste {
        terminal_id: terminal_id.clone(),
        event: PasteEvent {
            trust: PasteTrust::Trusted,
            data,
        },
    }
}

fn spawn_owned_by(request_id: u32, group: u32, owner: &WireResourceId) -> FrameKind {
    FrameKind::SpawnResource {
        request_id,
        group: GroupId::new(group),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: Some(owner.clone()),
        agent_session: None,
        initial_size: None,
        resource: None,
    }
}

fn whoami(request_id: u32) -> FrameKind {
    FrameKind::GetMetadata {
        request_id,
        scope: Scope::Global,
        key: WHOAMI_KEY.to_owned(),
    }
}

struct Outcome {
    frames: Vec<FrameKind>,
    closed: bool,
}

impl Outcome {
    fn result(&self, id: u32) -> Option<&CommandResult> {
        self.frames.iter().find_map(|frame| match frame {
            FrameKind::CommandResult { request_id, result } if *request_id == id => Some(result),
            _ => None,
        })
    }

    fn spawned(&self, id: u32) -> Option<&SpawnResult> {
        self.frames.iter().find_map(|frame| match frame {
            FrameKind::ResourceSpawned { request_id, result } if *request_id == id => Some(result),
            _ => None,
        })
    }

    fn whoami(&self, id: u32) -> serde_json::Value {
        let value = self
            .frames
            .iter()
            .find_map(|frame| match frame {
                FrameKind::MetadataValue { request_id, value } if *request_id == id => {
                    value.clone()
                }
                _ => None,
            })
            .expect("a whoami record");
        serde_json::from_slice(&value).unwrap()
    }

    fn denials(&self, request_id: Option<u32>) -> usize {
        self.frames
            .iter()
            .filter(|frame| is_denial(frame, request_id))
            .count()
    }

    fn ponged(&self, nonce: u64) -> bool {
        self.frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Pong { nonce: n } if *n == nonce))
    }

    fn detached(&self) -> bool {
        self.frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Detached { .. }))
    }
}

fn is_denial(frame: &FrameKind, request: Option<u32>) -> bool {
    matches!(
        frame,
        FrameKind::Error {
            request_id,
            code: ErrorCode::PermissionDenied,
            ..
        } if *request_id == request
    )
}

fn is_permission_denied(result: Option<&CommandResult>) -> bool {
    matches!(
        result,
        Some(CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            ..
        })
    )
}

/// Run the client loop over `frames` until `done` holds for what the server
/// wrote, the loop ends, or five seconds pass. The outcome is snapshotted
/// before the connection is cancelled, so teardown frames never count.
async fn exchange(
    state: &SharedState,
    frames: Vec<BytesMut>,
    lane: Option<InputLaneHandle>,
    done: impl Fn(&[FrameKind]) -> bool,
) -> Outcome {
    let wire = Wire::default();
    let root = CancellationToken::new();
    let connection = root.child_token();
    let task = tokio::task::spawn_local(handle_client(
        Feed::scripted(frames),
        wire.clone(),
        state.clone(),
        CLIENT,
        connection.clone(),
        root.clone(),
        lane,
        TransportType::Quic,
        false,
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(&wire.frames.borrow()) && !task.is_finished() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let outcome = Outcome {
        frames: wire.frames.borrow().clone(),
        closed: wire.closed.get(),
    };
    connection.cancel();
    let _ = task.await;
    outcome
}

fn has_result(id: u32) -> impl Fn(&[FrameKind]) -> bool {
    move |frames| {
        frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::CommandResult { request_id, .. } if *request_id == id))
    }
}

fn has_metadata(id: u32) -> impl Fn(&[FrameKind]) -> bool {
    move |frames| {
        frames.iter().any(
            |frame| matches!(frame, FrameKind::MetadataValue { request_id, .. } if *request_id == id),
        )
    }
}
