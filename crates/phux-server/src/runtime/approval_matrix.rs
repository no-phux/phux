//! Server-held approvals as clients meet them on the wire (ADR-0128,
//! `docs/spec/workload-auth.md` §6.1).
//!
//! Each test runs real client loops (`handle_client`) over transports the
//! test feeds as it goes, so a decision can name the id the server minted.
//! An owner connection subscribed to every event observes the journal: it
//! sees `approval_requested` and `approval_decided` with the actor each
//! carries, exactly as `phux watch` would.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use chrono::Utc;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, LayerSet};
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
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::policy::{ConnectionGrant, GrantFuture, PolicyEngine, PolicyError, ScopedPolicy};
use crate::state::{ClientId, SharedState};
use crate::transport::{FrameReader, FrameWriter};

const REQUESTER: ClientId = ClientId(31);
const APPROVER: ClientId = ClientId(32);
const OBSERVER: ClientId = ClientId(33);
const OUTSIDER: ClientId = ClientId(34);
const UNSIGNALED: ClientId = ClientId(35);

/// The held request's id in every test.
const HELD: u32 = 7;

/// Frames a test feeds one connection as it goes. Dropping the sender is
/// the peer's EOF.
struct Feed(mpsc::UnboundedReceiver<BytesMut>);

impl FrameReader for Feed {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        Ok(self.0.recv().await)
    }
}

/// Everything the server writes to one connection, decoded.
#[derive(Clone, Default)]
struct Wire(Rc<RefCell<Vec<FrameKind>>>);

#[allow(
    clippy::unused_async_trait_impl,
    reason = "the recording test writer implements the production async transport trait without I/O"
)]
impl FrameWriter for Wire {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        let decoded = FrameKind::decode(frame).expect("server frame").0;
        self.0.borrow_mut().push(decoded);
        Ok(())
    }

    async fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

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
        let (_, _, alpha) = s.seed_session("alpha");
        let (_, _, beta) = s.seed_session("beta");
        (s.intern_terminal_wire(alpha), s.intern_terminal_wire(beta))
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
            s.set_connection_identity(*client, workload_identity(&credential_id(*client)));
        }
    });
    World { state, alpha, beta }
}

/// The owner's socket: the serving uid over the Unix socket.
fn owner_peer() -> PeerIdentity {
    PeerIdentity {
        uid: nix::unistd::geteuid().as_raw(),
        pid: None,
        exe_path: None,
        mcp_host_key: None,
        transport: TransportType::UnixSocket,
        source_addr: None,
    }
}

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
            scopes: Vec::new(),
            issued_at: Utc::now(),
            expires_at: None,
            generation: 1,
            registry_instance: None,
        }),
        ssh_origin: None,
        bearer: None,
    }
}

fn hello() -> FrameKind {
    FrameKind::Hello {
        client_name: "approval-matrix".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::new().with_layers(LayerSet::all()),
    }
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
        Feed(frames),
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
    peer.send(&hello());
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
        self.wire.0.borrow().clone()
    }

    async fn until(&self, done: impl Fn(&[FrameKind]) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done(&self.wire.0.borrow()) {
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

#[tokio::test(flavor = "current_thread")]
async fn held_signal_writes_an_approval_record_emits_requested_and_defers_the_result() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;

            let record = record(&world.state, id).expect("the record is written");
            let WireResourceId::Local { id: alpha } = world.alpha else {
                unreachable!()
            };
            assert_eq!(record["id"], id.to_string());
            assert_eq!(record["method"], "KILL_RESOURCE");
            assert_eq!(
                record["subjects"],
                serde_json::json!([format!("terminal:{alpha}")])
            );
            assert_eq!(record["requester"]["client"], REQUESTER.0);
            assert_eq!(
                record["requester"]["credential_id"],
                credential_id(REQUESTER)
            );
            let ttl = record["expires_at_ms"].as_u64().unwrap()
                - record["requested_at_ms"].as_u64().unwrap();
            assert_eq!(ttl, 120_000, "the default TTL");

            assert!(
                watch.until(|frames| saw_requested(frames, id)).await,
                "approval_requested is journaled: {:?}",
                watch.frames()
            );
            let (_, actor) = watch
                .approval_events()
                .into_iter()
                .find(|(event, _)| matches!(event, AgentEvent::ApprovalRequested { .. }))
                .unwrap();
            assert_eq!(
                actor,
                Some(u32::try_from(REQUESTER.0).unwrap()),
                "the requester is the actor"
            );

            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(requester.results(HELD).is_empty(), "the result is deferred");
            requester.close().await;
            watch.close().await;
        })
        .await;
}

fn saw_requested(frames: &[FrameKind], id: ApprovalId) -> bool {
    approval_events(frames)
        .iter()
        .any(|(event, _)| *event == AgentEvent::ApprovalRequested { id })
}

#[tokio::test(flavor = "current_thread")]
async fn approve_by_a_signal_holder_runs_the_held_command_exactly_once_under_the_requesters_grant()
{
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&detach_all(HELD));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let result = requester.result(HELD).await;
            assert!(
                matches!(result, CommandResult::OkWith(_)),
                "the held command ran: {result:?}"
            );
            let approver_id = u32::try_from(APPROVER.0).unwrap();
            assert_eq!(
                watch.saw_decision(ApprovalOutcome::Approved).await,
                Some(approver_id),
                "approval_decided carries the approver"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(requester.results(HELD).len(), 1, "it ran exactly once");
            assert!(record(&world.state, id).is_none(), "the record is gone");
            assert!(
                approver
                    .frames()
                    .iter()
                    .all(|frame| !matches!(frame, FrameKind::Error { .. })),
                "the decision was accepted"
            );

            // The approval never lends the approver's grant: re-classified
            // under the requester's grant as it stands at decision time, a
            // requester that lost SIGNAL is refused though the approver
            // holds it at Global.
            requester.send(&detach_all(HELD + 1));
            let id = the_pending_approval(&world.state).await;
            let narrowed = ConnectionGrant::scoped(
                TerminalScopeSet::parse_all(&["observe@global"]).unwrap(),
                None,
            )
            .unwrap();
            world
                .state
                .with_mut(|s| s.set_connection_grant(REQUESTER, narrowed));
            approver.send(&decision(2, id, APPROVAL_APPROVE));
            let refused = requester.result(HELD + 1).await;
            assert!(is_denied(&refused, "permission denied"), "{refused:?}");

            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn deny_is_a_normal_journaled_outcome_with_permission_denied_to_the_requester() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(1, id, APPROVAL_DENY));
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "approval denied"), "{result:?}");
            assert_eq!(
                watch.saw_decision(ApprovalOutcome::Denied).await,
                Some(u32::try_from(APPROVER.0).unwrap())
            );
            assert!(record(&world.state, id).is_none());
            assert!(
                world.state.with(|s| s.registry().terminal_count()) == 2,
                "nothing was killed"
            );
            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_approve_of_the_same_id_is_rejected_single_use() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;
            let owner = connect(&world.state, OBSERVER, TransportType::UnixSocket).await;

            requester.send(&detach_all(HELD));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let _ = requester.result(HELD).await;

            // A scoped decider meets the guard: an id with no pending
            // approval resolves to no subject, refused like an absent one.
            approver.send(&decision(2, id, APPROVAL_APPROVE));
            assert_eq!(approver.refused(2).await, Some(ErrorCode::PermissionDenied));
            // The owner passes the guard and meets the table: still refused.
            owner.send(&decision(3, id, APPROVAL_APPROVE));
            assert_eq!(owner.refused(3).await, Some(ErrorCode::PermissionDenied));

            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(requester.results(HELD).len(), 1, "the command ran once");
            requester.close().await;
            approver.close().await;
            owner.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn expiry_denies_and_removes_the_record() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            world
                .state
                .with_mut(|s| s.set_approval_limits(Duration::from_millis(150), 64, 1024));
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "approval expired"), "{result:?}");
            assert!(record(&world.state, id).is_none(), "the record is removed");
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            assert_eq!(
                watch.saw_decision(ApprovalOutcome::Expired).await,
                None,
                "an expiry has no actor"
            );
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            assert_eq!(approver.refused(1).await, Some(ErrorCode::PermissionDenied));
            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn requester_disconnect_drops_its_holds() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let mut requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&kill(HELD, &world.alpha));
            requester.send(&detach_all(HELD + 1));
            assert!(until_state(&world.state, |s| s.pending_approval_ids().len() == 2).await);
            let ids = world
                .state
                .with(crate::state::ServerState::pending_approval_ids);

            requester.disconnect();
            assert!(
                until_state(&world.state, |s| s.pending_approval_ids().is_empty()).await,
                "a disconnect withdraws every hold"
            );
            for id in ids {
                assert!(record(&world.state, id).is_none());
            }
            assert_eq!(watch.saw_decision(ApprovalOutcome::Withdrawn).await, None);
            let withdrawn = watch
                .approval_events()
                .iter()
                .filter(|(event, _)| {
                    matches!(
                        event,
                        AgentEvent::ApprovalDecided {
                            outcome: ApprovalOutcome::Withdrawn,
                            ..
                        }
                    )
                })
                .count();
            assert_eq!(withdrawn, 2);
            requester.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_viewer_or_non_signal_holder_cannot_decide() {
    LocalSet::new()
        .run_until(async {
            let world = world(|_, beta| {
                vec![
                    (REQUESTER, vec!["observe,bind,?signal@global".to_owned()]),
                    // Un-held SIGNAL everywhere, but subscribed to the held
                    // Terminal as a VIEWER (ADR-0127).
                    (APPROVER, vec!["observe,bind,signal@global".to_owned()]),
                    // Un-held SIGNAL, on another Terminal only.
                    (
                        OUTSIDER,
                        vec![format!("observe,bind,signal@terminal:{beta}")],
                    ),
                    // No SIGNAL at all.
                    (UNSIGNALED, vec!["observe,bind@global".to_owned()]),
                ]
            });
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let viewer = connect(&world.state, APPROVER, TransportType::Quic).await;
            let outsider = connect(&world.state, OUTSIDER, TransportType::Quic).await;
            let unsignaled = connect(&world.state, UNSIGNALED, TransportType::Quic).await;
            world
                .state
                .with_mut(|s| s.set_viewer_mark(APPROVER, &world.alpha, true));

            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            for (peer, who) in [
                (&viewer, "an attach VIEWER, whatever its grant"),
                (&outsider, "SIGNAL on another Terminal"),
                (&unsignaled, "a grant without SIGNAL"),
                (&requester, "the requester, whose SIGNAL is held"),
            ] {
                peer.send(&decision(9, id, APPROVAL_APPROVE));
                assert_eq!(
                    peer.refused(9).await,
                    Some(ErrorCode::PermissionDenied),
                    "{who}"
                );
            }
            assert!(record(&world.state, id).is_some(), "still pending");
            assert!(requester.results(HELD).is_empty());

            // The viewer mark is what refused it: cleared, the same grant
            // decides.
            world
                .state
                .with_mut(|s| s.set_viewer_mark(APPROVER, &world.alpha, false));
            viewer.send(&decision(10, id, APPROVAL_DENY));
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "approval denied"), "{result:?}");
            let _ = world.beta;
            for peer in [requester, viewer, outsider, unsignaled] {
                peer.close().await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_mode_never_holds() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            state.with_mut(|s| {
                s.set_policy_engine(Arc::new(ScopedPolicy::local()));
                s.set_peer_identity(OBSERVER, owner_peer());
            });
            let owner = connect(&state, OBSERVER, TransportType::UnixSocket).await;
            owner.send(&detach_all(HELD));
            let result = owner.result(HELD).await;
            assert!(matches!(result, CommandResult::OkWith(_)), "{result:?}");
            assert!(state.with(|s| s.pending_approval_ids().is_empty()));
            owner.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_bound_refuses_the_65th_hold_with_resource_exhausted() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            for request_id in 1..=65 {
                requester.send(&detach_all(request_id));
            }
            let refused = requester.result(65).await;
            assert!(
                matches!(
                    refused,
                    CommandResult::Error {
                        code: ErrorCode::ResourceExhausted,
                        ..
                    }
                ),
                "{refused:?}"
            );
            assert_eq!(world.state.with(|s| s.pending_approval_ids().len()), 64);
            assert!((1..=64).all(|id| requester.results(id).is_empty()));
            requester.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_held_signal_on_a_frame_is_refused_not_held() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&FrameKind::SetMetadata {
                request_id: 4,
                scope: Scope::Global,
                key: CONFIG_RELOAD_KEY.to_owned(),
                value: b"nonce".to_vec(),
            });
            assert_eq!(
                requester.refused(4).await,
                Some(ErrorCode::PermissionDenied)
            );
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            requester.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revocation_withdraws_the_requesters_holds() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            world.state.with_mut(|s| {
                let _ = super::client::release_revoked_consumer_state(s, REQUESTER);
            });
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            assert!(record(&world.state, id).is_none());
            assert_eq!(watch.saw_decision(ApprovalOutcome::Withdrawn).await, None);
            requester.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_viewer_cannot_decide_a_held_close_tab_and_its_record_lists_the_terminals() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let viewer = connect(&world.state, APPROVER, TransportType::Quic).await;
            world
                .state
                .with_mut(|s| s.set_viewer_mark(APPROVER, &world.alpha, true));
            requester.send(&command(
                HELD,
                Command::CloseTabResources {
                    ids: vec![world.alpha.clone(), world.beta.clone()],
                },
            ));
            let id = the_pending_approval(&world.state).await;
            let (WireResourceId::Local { id: alpha }, WireResourceId::Local { id: beta }) =
                (&world.alpha, &world.beta)
            else {
                unreachable!()
            };
            let written = record(&world.state, id).expect("the record is written");
            assert_eq!(written["method"], "CLOSE_TAB_RESOURCES");
            assert_eq!(
                written["subjects"],
                serde_json::json!([format!("terminal:{alpha}"), format!("terminal:{beta}")])
            );
            viewer.send(&decision(1, id, APPROVAL_APPROVE));
            assert_eq!(viewer.refused(1).await, Some(ErrorCode::PermissionDenied));
            assert!(record(&world.state, id).is_some(), "still pending");
            requester.close().await;
            viewer.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_server_wide_bound_refuses_with_resource_exhausted() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            world
                .state
                .with_mut(|s| s.set_approval_limits(Duration::from_secs(120), 64, 2));
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            for request_id in 1..=3 {
                requester.send(&detach_all(request_id));
            }
            let refused = requester.result(3).await;
            assert!(
                matches!(
                    &refused,
                    CommandResult::Error { code: ErrorCode::ResourceExhausted, message }
                        if message.contains("on this server")
                ),
                "{refused:?}"
            );
            assert_eq!(world.state.with(|s| s.pending_approval_ids().len()), 2);
            requester.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_reaped_terminal_withdraws_its_holds_before_its_close() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&kill(HELD, &world.alpha));
            let _ = the_pending_approval(&world.state).await;
            world.state.with_mut(|s| {
                super::client::journal_pane_closed(
                    s,
                    &world.alpha,
                    None,
                    None,
                    crate::state::CloseAttribution::default(),
                );
            });
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "terminal gone"), "{result:?}");
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            let closed = |frames: &[FrameKind]| {
                frames.iter().position(|frame| {
                    matches!(
                        frame,
                        FrameKind::Event {
                            event: AgentEvent::ResourceClosed { .. },
                            ..
                        }
                    )
                })
            };
            assert!(watch.until(|frames| closed(frames).is_some()).await);
            let frames = watch.frames();
            let decided = frames.iter().position(|frame| {
                matches!(
                    frame,
                    FrameKind::Event {
                        event: AgentEvent::ApprovalDecided {
                            outcome: ApprovalOutcome::Withdrawn,
                            ..
                        },
                        ..
                    }
                )
            });
            assert!(
                decided.is_some() && decided < closed(&frames),
                "approval_decided precedes pane_closed: {frames:?}"
            );
            requester.close().await;
            watch.close().await;
        })
        .await;
}

/// Held commands are SIGNAL-class, and every SIGNAL-class command routes to
/// the handler, as an approved one does.
#[test]
fn every_signal_class_command_routes_to_the_handler() {
    use phux_protocol::wire::frame::TerminalSignal;
    let terminal = WireResourceId::local(3);
    for held in [
        Command::KillResource {
            terminal_id: terminal.clone(),
            operation_id: None,
        },
        Command::KillResources {
            ids: vec![terminal.clone()],
            operation_id: None,
        },
        Command::CloseTabResources {
            ids: vec![terminal.clone()],
        },
        Command::SignalTerminal {
            terminal_id: terminal,
            signal: TerminalSignal::Kill,
            operation_id: None,
        },
        Command::DetachClients { session: None },
        Command::Upgrade,
    ] {
        for lane in [false, true] {
            assert_eq!(
                super::client::route(&held, lane),
                super::client::Route::Handler,
                "{held:?}"
            );
        }
    }
}

/// A keyed `KILL_RESOURCES` of an id that is not here: it answers OK and
/// binds its key (L20), so it shows what a keyed retry replays.
fn keyed_kill(request_id: u32, key: u8) -> FrameKind {
    command(
        request_id,
        Command::KillResources {
            ids: vec![WireResourceId::local(999)],
            operation_id: phux_protocol::ids::IdempotencyKey::new([key; 16]),
        },
    )
}

fn requested_count(frames: &[FrameKind]) -> usize {
    approval_events(frames)
        .iter()
        .filter(|(event, _)| matches!(event, AgentEvent::ApprovalRequested { .. }))
        .count()
}

/// ADR-0128 with L20: an approved keyed kill retried with its key replays
/// the first answer, without a second hold or a second delivery.
#[tokio::test(flavor = "current_thread")]
async fn an_approved_keyed_kill_retried_with_its_key_replays_without_a_second_hold() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&keyed_kill(HELD, 0x5a));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let first = requester.result(HELD).await;
            assert!(matches!(first, CommandResult::OkWith(_)), "{first:?}");

            requester.send(&keyed_kill(HELD + 1, 0x5a));
            let replayed = requester.result(HELD + 1).await;
            assert_eq!(replayed, first, "the retry replays the first answer");
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(requested_count(&watch.frames()), 1, "one hold only");
            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

/// ADR-0128 with L20: two identical keyed requests held at once share one
/// approval record; one decision answers both.
#[tokio::test(flavor = "current_thread")]
async fn two_identical_keyed_requests_held_concurrently_share_one_approval() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&keyed_kill(HELD, 0x6b));
            requester.send(&keyed_kill(HELD + 1, 0x6b));
            let id = the_pending_approval(&world.state).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(
                world.state.with(|s| s.pending_approval_ids().len()),
                1,
                "the repeat joined the pending hold"
            );
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let first = requester.result(HELD).await;
            let second = requester.result(HELD + 1).await;
            assert!(matches!(first, CommandResult::OkWith(_)), "{first:?}");
            assert_eq!(second, first, "both waiters get the one execution's answer");
            assert_eq!(requested_count(&watch.frames()), 1);
            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

/// A joined waiter counts against the per-connection bound like a hold, and
/// its count returns when the approval resolves.
#[tokio::test(flavor = "current_thread")]
async fn joins_past_the_per_connection_bound_are_refused_and_released_after() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            world
                .state
                .with_mut(|s| s.set_approval_limits(Duration::from_secs(120), 2, 1024));
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&keyed_kill(HELD, 0x7c));
            let id = the_pending_approval(&world.state).await;
            requester.send(&keyed_kill(HELD + 1, 0x7c));
            assert!(
                until_state(&world.state, |s| s.approvals_held_by(REQUESTER) == 2).await,
                "the join counts"
            );
            requester.send(&keyed_kill(HELD + 2, 0x7c));
            let refused = requester.result(HELD + 2).await;
            assert!(
                matches!(
                    &refused,
                    CommandResult::Error { code: ErrorCode::ResourceExhausted, message }
                        if message == "too many actions awaiting approval"
                ),
                "{refused:?}"
            );
            assert_eq!(world.state.with(|s| s.pending_approval_ids().len()), 1);

            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let _ = requester.result(HELD).await;
            let _ = requester.result(HELD + 1).await;
            assert!(
                until_state(&world.state, |s| s.approvals_held_by(REQUESTER) == 0).await,
                "every count is released"
            );
            requester.close().await;
            approver.close().await;
        })
        .await;
}
