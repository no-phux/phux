//! Server-held approvals as clients meet them on the wire (ADR-0128,
//! `docs/agents.md#safety`). An owner connection subscribed to every event
//! observes the journal, as `phux watch` would.

use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_protocol::ids::{ApprovalId, ResourceId as WireResourceId};
use phux_protocol::policy::TransportType;
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    APPROVAL_APPROVE, APPROVAL_DENY, AgentEvent, ApprovalOutcome, CONFIG_RELOAD_KEY, Command,
    CommandResult, ErrorCode, FrameKind, Scope,
};
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;

use super::test_support::{
    Connection as Peer, PATIENCE, error_code, owner_peer, spawn_connection, workload_identity,
};
use crate::policy::testing::{FnPolicy, scoped_grant};
use crate::policy::{ConnectionGrant, PolicyError, ScopedPolicy};
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
fn credential_grants(known: Vec<(String, TerminalScopeSet)>) -> FnPolicy {
    FnPolicy::new(move |_, credential| {
        let Some(credential) = credential else {
            return Ok(ConnectionGrant::owner());
        };
        let (_, set) = known
            .iter()
            .find(|(id, _)| *id == credential.id)
            .ok_or_else(|| PolicyError::Unauthorized("unknown credential".to_owned()))?;
        scoped_grant(set, Some(credential))
    })
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
    let engine = credential_grants(
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

async fn connect(state: &SharedState, client: ClientId, transport: TransportType) -> Peer {
    let root = CancellationToken::new();
    let peer = spawn_connection(state, client, transport, &root, None, true);
    peer.hello().await;
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

/// Whether `done` holds of the server state within the patience window.
async fn until_state(
    state: &SharedState,
    done: impl Fn(&crate::state::ServerState) -> bool,
) -> bool {
    let deadline = Instant::now() + PATIENCE;
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

mod audit;
mod holds;
mod refusals;
mod routing;
mod verbs;
