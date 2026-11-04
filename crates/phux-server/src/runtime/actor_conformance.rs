//! Actors over the real client loop (ADR-0163): a delegated agent is
//! refused what its grant lacks, a human's takeover from an agent always
//! wins and the agent hears about it, an agent never seizes a human's
//! lease, and a minted credential's own socket admits connections as it.
//!
//! The connection cases run `handle_client` over a scripted transport, as
//! the revocation conformance sweep does; the socket case dials a real
//! delegated socket.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use chrono::Utc;
use phux_core::PrincipalId;
use phux_protocol::ids::{ResourceId as WireResourceId, ServerInstance};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, ControlAction, ErrorCode, FrameKind,
    InputMode, LiveTarget, Refusal, TerminateMode, ViewportInfo,
};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, LocalSet};
use tokio_util::sync::CancellationToken;

use super::delegation::{delegate_agent, revoke_delegated_agent, stamped_identity};
use super::test_support::{Feed, Wire, encode, hello, owner_peer, workload_identity};
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::delegation::{DelegatedCredential, DelegationRequest};
use crate::policy::{ConnectionGrant, GrantFuture, PolicyEngine, PolicyError};
use crate::runtime::connections::handle_client;
use crate::state::{ClientId, ServerState, SharedState};

/// How long any one expectation may take before the case fails.
const PATIENCE: Duration = Duration::from_secs(5);

/// The automation credential the non-human, non-delegated cases use.
const AUTOMATION: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A paired phone's credential: enrolled for a person.
const PHONE: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The owner's socket gets the owner's grant; any other verified credential
/// gets every verb at Global as itself: a non-human principal whose grant
/// does not stand in the way, so only lease precedence decides. [`PHONE`]
/// is a human principal, as a paired device's registry record makes it.
#[derive(Debug)]
struct Automation;

impl PolicyEngine for Automation {
    fn authorize_hello<'a>(
        &'a self,
        peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let grant = if matches!(peer_identity.transport, TransportType::UnixSocket) {
            Ok(ConnectionGrant::owner())
        } else {
            let phone = credential.is_some_and(|credential| credential.id == PHONE);
            ConnectionGrant::scoped(
                TerminalScopeSet::parse_all(&["*@global"]).unwrap(),
                credential.map(|credential| credential.id.clone()),
            )
            .map(|grant| {
                if phone {
                    grant.with_kind(phux_core::actor::PrincipalKind::Human)
                } else {
                    grant
                }
            })
            .map_err(|error| PolicyError::Internal(error.to_string()))
        };
        Box::pin(async move { grant })
    }
}

/// A server with one session, `alpha`, whose pane has a live (no-PTY)
/// actor.
struct Fixture {
    state: SharedState,
    root: CancellationToken,
    alpha: WireResourceId,
    alpha_core: phux_core::ids::ResourceId,
    alpha_session: phux_core::ids::SessionId,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.root.cancel();
    }
}

impl Fixture {
    fn new() -> Self {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let alpha_core = super::commands::seed_session_with_actor(
            &state,
            "alpha",
            phux_config::ScrollbackLimits::default(),
            &root,
        )
        .unwrap();
        let (alpha, alpha_session) = state.with_mut(|s| {
            s.set_policy_engine(Arc::new(Automation));
            let session = s.registry().session_of(alpha_core).unwrap();
            (s.intern_resource_wire(alpha_core), session)
        });
        Self {
            state,
            root,
            alpha,
            alpha_core,
            alpha_session,
        }
    }

    fn connect(&self, identity: ConnectionIdentity) -> Connection {
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
            None,
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

    /// The owner over its Unix socket, past `HELLO`.
    async fn human(&self) -> Connection {
        let connection = self.connect(owner_peer().into());
        connection.hello().await;
        connection
    }

    /// An automation credential over QUIC, past `HELLO`.
    async fn automation(&self) -> Connection {
        let connection = self.connect(workload_identity(AUTOMATION, &["*@global"]));
        connection.hello().await;
        connection
    }

    /// A paired phone over QUIC, past `HELLO`.
    async fn phone(&self) -> Connection {
        let connection = self.connect(workload_identity(PHONE, &["*@global"]));
        connection.hello().await;
        connection
    }

    /// A credential `launcher` delegates on `alpha`.
    fn delegate(&self, launcher: &Connection) -> DelegatedCredential {
        self.state
            .with_mut(|s| {
                s.mint_delegation(
                    launcher.client,
                    self.alpha_session,
                    DelegationRequest::default(),
                    Utc::now(),
                )
            })
            .unwrap()
    }

    /// The agent `launcher` delegated, connected through its credential.
    async fn delegated_agent(&self, launcher: &Connection) -> (Connection, DelegatedCredential) {
        let credential = self.delegate(launcher);
        let connection = self.connect(stamped_identity(owner_peer(), &credential));
        connection.hello().await;
        (connection, credential)
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
        let _ = self.feed.send(encode(frame));
    }

    fn frames(&self) -> Vec<FrameKind> {
        self.wire.frames.borrow().clone()
    }

    async fn hello(&self) {
        self.send(&hello("actor-conformance"));
        self.wait_for("HELLO_OK", |frame| {
            matches!(frame, FrameKind::HelloOk { .. })
        })
        .await;
    }

    async fn attach(&self) {
        self.send(&FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::ByName("alpha".to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        });
        self.wait_for("ATTACHED", |frame| {
            matches!(frame, FrameKind::Attached { .. })
        })
        .await;
    }

    /// Send `command` and return its result.
    async fn command(&self, request_id: u32, command: Command) -> CommandResult {
        self.send(&FrameKind::Command {
            request_id,
            command,
        });
        self.wait_for("COMMAND_RESULT", |frame| {
            matches!(frame, FrameKind::CommandResult { request_id: id, .. } if *id == request_id)
        })
        .await;
        self.frames()
            .into_iter()
            .find_map(|frame| match frame {
                FrameKind::CommandResult {
                    request_id: id,
                    result,
                } if id == request_id => Some(result),
                _ => None,
            })
            .unwrap()
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

    async fn finish(self) {
        self.token.cancel();
        let _ = self.task.await;
    }
}

fn acquire(terminal_id: &WireResourceId, mode: InputMode) -> Command {
    Command::AcquireInput {
        terminal_id: terminal_id.clone(),
        mode,
        ttl_ms: 0,
    }
}

fn principal_of(principal: &PrincipalId) -> Option<phux_protocol::ids::PrincipalId> {
    crate::org_wire::principal_id(principal)
}

/// Run `case` on a current-thread runtime's `LocalSet`, as the server does.
async fn local(case: impl std::future::Future<Output = ()>) {
    LocalSet::new().run_until(case).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_delegated_agent_acts_as_its_credential_on_behalf_of_the_owner() {
    local(async {
        let fx = Fixture::new();
        let human = fx.human().await;
        let (agent, credential) = fx.delegated_agent(&human).await;

        let actor = fx.state.with(|s| s.actor_for(agent.client));
        assert_eq!(actor.principal, credential.principal());
        assert_eq!(actor.on_behalf_of, Some(PrincipalId::Owner));
        assert_eq!(
            fx.state.with(|s| s.actor_for(human.client)).principal,
            PrincipalId::Owner
        );
        agent.finish().await;
        human.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_delegated_agent_is_refused_terminate_and_kill_and_its_pane_survives() {
    local(async {
        let fx = Fixture::new();
        let human = fx.human().await;
        let (agent, _) = fx.delegated_agent(&human).await;

        let killed = agent
            .command(
                1,
                Command::KillResource {
                    terminal_id: fx.alpha.clone(),
                    operation_id: None,
                },
            )
            .await;
        assert!(
            matches!(
                killed,
                CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    ..
                }
            ),
            "{killed:?}"
        );
        let terminated = agent
            .command(
                2,
                Command::ResourceTerminate {
                    operation: phux_protocol::ids::OperationId::new([3; 16]).unwrap(),
                    target: LiveTarget {
                        resource: fx.alpha.clone(),
                        instance: ServerInstance::new([0; 16]),
                        activation: phux_protocol::ids::ActivationId::new(1),
                    },
                    mode: TerminateMode::Force,
                },
            )
            .await;
        assert!(
            matches!(
                terminated,
                CommandResult::Refused(Refusal::Unauthorized { .. })
            ),
            "{terminated:?}"
        );
        assert!(
            fx.state
                .with(|s| s.registry().record(fx.alpha_core).is_some()),
            "the pane survives"
        );
        agent.finish().await;
        human.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_human_takeover_from_an_agent_always_succeeds_and_the_agent_hears_it() {
    local(async {
        let fx = Fixture::new();
        let agent = fx.automation().await;
        agent.attach().await;
        agent.send(&FrameKind::SubscribeEvents {
            terminal: Some(fx.alpha.clone()),
            after_seq: None,
        });
        assert_eq!(
            agent
                .command(1, acquire(&fx.alpha, InputMode::Cooperative))
                .await,
            CommandResult::Ok
        );
        assert_eq!(fx.lease_holder(), Some(agent.client));
        let before = fx.state.with(|s| s.input_lease_epoch(fx.alpha_core));

        let human = fx.human().await;
        human.attach().await;
        assert_eq!(
            human.command(1, acquire(&fx.alpha, InputMode::Seize)).await,
            CommandResult::Ok
        );
        assert_eq!(fx.lease_holder(), Some(human.client));
        let after = fx.state.with(|s| s.input_lease_epoch(fx.alpha_core));
        assert_eq!(after.get(), before.get() + 1, "the takeover opens an epoch");

        let owner = principal_of(&PrincipalId::Owner);
        agent
            .wait_for("LeaseChanged to the human", |frame| {
                matches!(
                    frame,
                    FrameKind::Event {
                        event: AgentEvent::LeaseChanged { epoch, holder: Some(holder), .. },
                        ..
                    } if *epoch == after && holder.principal() == owner
                )
            })
            .await;
        agent
            .wait_for("the lease-lost terminal_control", |frame| {
                matches!(
                    frame,
                    FrameKind::Event {
                        event: AgentEvent::TerminalControl {
                            action: ControlAction::Seized,
                            input_holder: Some(holder),
                            ..
                        },
                        ..
                    } if holder.get() == u32::try_from(human.client.0).unwrap()
                )
            })
            .await;
        agent.finish().await;
        human.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_agent_cannot_seize_a_human_held_lease() {
    local(async {
        let fx = Fixture::new();
        let human = fx.human().await;
        human.attach().await;
        assert_eq!(
            human
                .command(1, acquire(&fx.alpha, InputMode::Cooperative))
                .await,
            CommandResult::Ok
        );
        let epoch = fx.state.with(|s| s.input_lease_epoch(fx.alpha_core));

        let agent = fx.automation().await;
        agent.attach().await;
        let seized = agent.command(1, acquire(&fx.alpha, InputMode::Seize)).await;
        assert!(
            matches!(
                seized,
                CommandResult::Error {
                    code: ErrorCode::InputLeaseHeld,
                    ..
                }
            ),
            "{seized:?}"
        );
        assert_eq!(fx.lease_holder(), Some(human.client));
        assert_eq!(
            fx.state.with(|s| s.input_lease_epoch(fx.alpha_core)),
            epoch,
            "a refused seize changes nothing"
        );

        // A takeover attach leaves the human's lease alone too.
        let effects = fx.state.with_mut(|s| {
            s.apply_attach_role(
                agent.client,
                &fx.alpha,
                Some(fx.alpha_core),
                phux_protocol::wire::frame::RolePolicy::TAKEOVER,
                true,
            )
        });
        assert_eq!(effects.lease, None);
        assert_eq!(fx.lease_holder(), Some(human.client));
        agent.finish().await;
        human.finish().await;
    })
    .await;
}

mod cases;
