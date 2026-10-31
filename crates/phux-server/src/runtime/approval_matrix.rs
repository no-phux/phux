//! Server-held approvals as clients meet them on the wire (ADR-0128,
//! `docs/agents.md#safety`).
//!
//! Each test runs real client loops (`handle_client`) over transports the
//! test feeds as it goes, so a decision can name the id the server minted.
//! An owner connection subscribed to every event observes the journal: it
//! sees `approval_requested` and `approval_decided` with the actor each
//! carries, exactly as `phux watch` would.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::ids::{ApprovalId, ResourceId as WireResourceId};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    APPROVAL_APPROVE, APPROVAL_DENY, AgentEvent, ApprovalOutcome, CONFIG_RELOAD_KEY, Command,
    CommandResult, ErrorCode, FrameKind, Scope,
};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, LocalSet};
use tokio_util::sync::CancellationToken;

use super::client::handle_client;
use super::test_support::{Feed, Wire, hello, owner_peer, workload_identity};
use crate::auth::AuthenticatedCredential;
use crate::policy::{ConnectionGrant, GrantFuture, PolicyEngine, PolicyError, ScopedPolicy};
use crate::state::{ClientId, SharedState};

const REQUESTER: ClientId = ClientId(31);
const APPROVER: ClientId = ClientId(32);
const OBSERVER: ClientId = ClientId(33);
const OUTSIDER: ClientId = ClientId(34);
const UNSIGNALED: ClientId = ClientId(35);

/// The held request's id in every test.
const HELD: u32 = 7;

/// Mints each credential's grant; a connection with no credential gets the
/// owner's grant.
#[derive(Debug)]
struct Grants(Vec<(String, TerminalScopeSet)>);

impl PolicyEngine for Grants {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let grant = credential.map_or_else(
            || Ok(ConnectionGrant::owner()),
            |credential| self.scoped(&credential.id),
        );
        Box::pin(async move { grant })
    }
}

impl Grants {
    fn scoped(&self, id: &str) -> Result<ConnectionGrant, PolicyError> {
        let (_, set) = self
            .0
            .iter()
            .find(|(known, _)| known == id)
            .ok_or_else(|| PolicyError::Unauthorized("unknown credential".to_owned()))?;
        ConnectionGrant::scoped(set.clone(), Some(id.to_owned()))
            .map_err(|err| PolicyError::Internal(err.to_string()))
    }
}

struct World {
    state: SharedState,
    alpha: WireResourceId,
    beta: WireResourceId,
}

fn credential_id(client: ClientId) -> String {
    format!("cred-{}", client.0)
}

/// Two sessions with one Terminal each. Every client `grants` names is a
/// workload holding that ceiling (the closure gets `(alpha, beta)` to name
/// Terminal selectors); any other client is the owner's socket.
fn world(grants: impl FnOnce(u32, u32) -> Vec<(ClientId, Vec<String>)>) -> World {
    let state = SharedState::new();
    let (alpha, beta) = state.with_mut(|s| {
        let (_, alpha) = s.seed_session("alpha");
        let (_, beta) = s.seed_session("beta");
        (s.intern_resource_wire(alpha), s.intern_resource_wire(beta))
    });
    let (WireResourceId::Local { id: a }, WireResourceId::Local { id: b }) = (&alpha, &beta) else {
        unreachable!("seeded panes are local");
    };
    let grants = grants(*a, *b);
    let engine = Grants(
        grants
            .iter()
            .map(|(client, scopes)| {
                let set = TerminalScopeSet::parse_all(scopes).unwrap();
                (credential_id(*client), set)
            })
            .collect(),
    );
    state.with_mut(|s| {
        s.set_policy_engine(Arc::new(engine));
        s.set_peer_identity(OBSERVER, owner_peer());
        for (client, _) in &grants {
            s.set_connection_identity(*client, workload_identity(&credential_id(*client), &[]));
        }
    });
    World { state, alpha, beta }
}

/// One live connection the test drives.
struct Peer {
    feed: Option<mpsc::UnboundedSender<BytesMut>>,
    wire: Wire,
    token: CancellationToken,
    task: JoinHandle<io::Result<()>>,
}

async fn connect(state: &SharedState, client: ClientId, transport: TransportType) -> Peer {
    let (feed, frames) = mpsc::unbounded_channel();
    let wire = Wire::default();
    let root = CancellationToken::new();
    let token = root.child_token();
    let task = tokio::task::spawn_local(handle_client(
        Feed::eof_on_close(frames),
        wire.clone(),
        state.clone(),
        client,
        token.clone(),
        root,
        None,
        transport,
        false,
    ));
    let peer = Peer {
        feed: Some(feed),
        wire,
        token,
        task,
    };
    peer.send(&hello("approval-matrix"));
    assert!(
        peer.until(|frames| frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::HelloOk { .. })))
            .await,
        "HELLO completes"
    );
    peer
}

/// An owner connection subscribed to every event from the journal's start.
async fn observer(state: &SharedState) -> Peer {
    let peer = connect(state, OBSERVER, TransportType::UnixSocket).await;
    peer.send(&FrameKind::SubscribeEvents {
        terminal: None,
        after_seq: Some(0),
    });
    peer
}

impl Peer {
    fn send(&self, frame: &FrameKind) {
        let mut out = BytesMut::new();
        frame.encode(&mut out);
        self.feed.as_ref().expect("connected").send(out).unwrap();
    }

    fn frames(&self) -> Vec<FrameKind> {
        self.wire.frames.borrow().clone()
    }

    async fn until(&self, done: impl Fn(&[FrameKind]) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done(&self.wire.frames.borrow()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        false
    }

    fn results(&self, id: u32) -> Vec<CommandResult> {
        self.frames()
            .into_iter()
            .filter_map(|frame| match frame {
                FrameKind::CommandResult { request_id, result } if request_id == id => Some(result),
                _ => None,
            })
            .collect()
    }

    async fn result(&self, id: u32) -> CommandResult {
        assert!(
            self.until(|frames| frames.iter().any(
                |frame| matches!(frame, FrameKind::CommandResult { request_id, .. } if *request_id == id)
            ))
            .await,
            "result {id} arrives: {:?}",
            self.frames()
        );
        self.results(id).remove(0)
    }

    /// Whether request `id` was refused with a correlated `ERROR`.
    async fn refused(&self, id: u32) -> Option<ErrorCode> {
        let refusal = |frames: &[FrameKind]| {
            frames.iter().find_map(|frame| match frame {
                FrameKind::Error {
                    request_id: Some(got),
                    code,
                    ..
                } if *got == id => Some(*code),
                _ => None,
            })
        };
        self.until(|frames| refusal(frames).is_some()).await;
        refusal(&self.frames())
    }

    /// `(event, actor client)` for every approval event this observer saw.
    fn approval_events(&self) -> Vec<(AgentEvent, Option<u32>)> {
        approval_events(&self.frames())
    }

    /// The actor of the first `approval_decided` with `outcome`, waiting
    /// for it to arrive.
    async fn saw_decision(&self, outcome: ApprovalOutcome) -> Option<u32> {
        let decided = |frames: &[FrameKind]| {
            approval_events(frames)
                .into_iter()
                .find_map(|(event, actor)| match event {
                    AgentEvent::ApprovalDecided { outcome: got, .. } if got == outcome => {
                        Some(actor)
                    }
                    _ => None,
                })
        };
        assert!(
            self.until(|frames| decided(frames).is_some()).await,
            "approval_decided {outcome:?} arrives: {:?}",
            self.frames()
        );
        decided(&self.frames()).flatten()
    }

    fn disconnect(&mut self) {
        self.feed = None;
    }

    async fn close(self) {
        self.token.cancel();
        let _ = self.task.await;
    }
}

fn approval_events(frames: &[FrameKind]) -> Vec<(AgentEvent, Option<u32>)> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Event { event, stamp, .. } if is_approval_event(event) => {
                let actor = stamp
                    .as_ref()
                    .and_then(|stamp| stamp.actor.as_ref().map(|actor| actor.client.get()));
                Some((event.clone(), actor))
            }
            _ => None,
        })
        .collect()
}

const fn is_approval_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::ApprovalRequested { .. } | AgentEvent::ApprovalDecided { .. }
    )
}

fn command(request_id: u32, command: Command) -> FrameKind {
    FrameKind::Command {
        request_id,
        command,
    }
}

fn detach_all(request_id: u32) -> FrameKind {
    command(request_id, Command::DetachClients { session: None })
}

fn kill(request_id: u32, terminal: &WireResourceId) -> FrameKind {
    command(
        request_id,
        Command::KillResource {
            terminal_id: terminal.clone(),
            operation_id: None,
        },
    )
}

fn decision(request_id: u32, id: ApprovalId, value: &[u8]) -> FrameKind {
    FrameKind::SetMetadata {
        request_id,
        scope: Scope::Global,
        key: id.decide_key(),
        value: value.to_vec(),
    }
}

async fn until_state(
    state: &SharedState,
    done: impl Fn(&crate::state::ServerState) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if state.with(&done) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

/// The one pending approval, once it exists.
async fn the_pending_approval(state: &SharedState) -> ApprovalId {
    assert!(
        until_state(state, |s| s.pending_approval_ids().len() == 1).await,
        "one action is held"
    );
    state.with(|s| s.pending_approval_ids()[0])
}

fn record(state: &SharedState, id: ApprovalId) -> Option<serde_json::Value> {
    state
        .with(|s| s.metadata().get(&Scope::Global, &id.record_key()))
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

fn is_denied(result: &CommandResult, message: &str) -> bool {
    matches!(
        result,
        CommandResult::Error { code: ErrorCode::PermissionDenied, message: got } if got == message
    )
}

fn held_signal_world() -> World {
    world(|_, _| {
        vec![
            (REQUESTER, vec!["observe,bind,?signal@global".to_owned()]),
            (APPROVER, vec!["observe,bind,signal@global".to_owned()]),
        ]
    })
}

mod holds;
mod refusals;
mod routing;
