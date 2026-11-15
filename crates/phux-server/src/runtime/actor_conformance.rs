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

use std::sync::Arc;

use chrono::Utc;
use phux_core::PrincipalId;
use phux_protocol::ids::{ResourceId as WireResourceId, ServerInstance};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ControlAction, ErrorCode, FrameKind, InputMode, LiveTarget,
    Refusal, TerminateMode,
};
use tokio::io::AsyncWriteExt as _;
use tokio_util::sync::CancellationToken;

use super::delegation::{delegate_agent, revoke_delegated_agent, stamped_identity};
use super::test_support::{
    Connection, acquire, alpha_world, encode, hello, local, owner_peer, spawn_connection,
    wait_until, workload_identity,
};
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::delegation::{DelegatedCredential, DelegationRequest};
use crate::policy::{ConnectionGrant, GrantFuture, PolicyEngine, PolicyError};
use crate::runtime::test_support::error_code;
use crate::state::{ClientId, ServerState, SharedState};

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
        let root = CancellationToken::new();
        let (state, alpha, alpha_core) = alpha_world(Arc::new(Automation), &root);
        let alpha_session = state.with(|s| s.registry().session_of(alpha_core).unwrap());
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
        spawn_connection(&self.state, client, transport, &self.root, None, false)
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

fn principal_of(principal: &PrincipalId) -> Option<phux_protocol::ids::PrincipalId> {
    crate::org_wire::principal_id(principal)
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
        assert_eq!(
            error_code(&killed),
            Some(ErrorCode::PermissionDenied),
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
        agent.attach(1).await;
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
        human.attach(1).await;
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
        human.attach(1).await;
        assert_eq!(
            human
                .command(1, acquire(&fx.alpha, InputMode::Cooperative))
                .await,
            CommandResult::Ok
        );
        let epoch = fx.state.with(|s| s.input_lease_epoch(fx.alpha_core));

        let agent = fx.automation().await;
        agent.attach(1).await;
        let seized = agent.command(1, acquire(&fx.alpha, InputMode::Seize)).await;
        assert_eq!(
            error_code(&seized),
            Some(ErrorCode::InputLeaseHeld),
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
