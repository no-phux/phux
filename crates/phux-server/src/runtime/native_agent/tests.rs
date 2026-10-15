//! Exercise the production broker/record actor boundary, without a provider
//! model or an echo adapter. Host credentials are real narrowed delegations.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "isolated broker regressions"
)]
use super::*;
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::policy::ConnectionGrant;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{Command, FrameKind, RESOURCE_ASKED_KEY, Scope};
use tokio::sync::mpsc;

struct Fixture {
    state: SharedState,
    root: CancellationToken,
    owner: ClientId,
    host: ClientId,
    parent: phux_core::ids::ResourceId,
    agent: ResourceId,
    parent_wire: ResourceId,
    dir: tempfile::TempDir,
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
        let dir = tempfile::tempdir().unwrap();
        let peer = PeerIdentity {
            uid: rustix::process::geteuid().as_raw(),
            pid: Some(std::process::id()),
            exe_path: None,
            mcp_host_key: None,
            transport: TransportType::UnixSocket,
            source_addr: None,
        };
        let recipe = SpawnNativeAgent {
            session: "alpha".into(),
            backend: "omp-rpc".into(),
            provider: "omp".into(),
            command: vec!["provider-not-started-in-broker-test".into()],
            cwd: None,
            env: BTreeMap::new(),
            sdk_runtime: None,
            node_bridge: None,
            native_id: None,
            guardian: None,
            guardian_key: None,
            guardian_expected: None,
        };
        let (owner, session, parent, parent_wire) =
            Self::create_guardian(&state, &root, dir.path(), &peer, &recipe);
        let delegation = super::super::delegation::delegate_agent(
            &state,
            owner,
            session,
            crate::delegation::DelegationRequest::default(),
            None,
            &root,
        )
        .unwrap();
        let host = state.with_mut(|s| {
            let host = s.new_client_id();
            let mut identity = ConnectionIdentity::from(peer);
            identity.credential = Some(AuthenticatedCredential {
                id: delegation.credential.id.clone(),
                principal: "delegated-agent".into(),
                scopes: delegation
                    .credential
                    .scopes()
                    .grants()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                issued_at: chrono::Utc::now(),
                expires_at: Some(delegation.credential.expires_at),
                generation: 0,
                registry_instance: None,
            });
            s.set_connection_identity(host, identity);
            s.set_connection_grant(
                host,
                ConnectionGrant::delegated(
                    delegation.credential.scopes(),
                    delegation.credential.principal(),
                    delegation.credential.on_behalf_of.clone(),
                    delegation.credential.expires_at,
                )
                .unwrap(),
            );
            host
        });
        let agent = Self::create_agent(
            &state,
            &root,
            owner,
            parent,
            &parent_wire,
            recipe,
            &delegation.credential.id,
        );
        Self {
            state,
            root,
            owner,
            host,
            parent,
            agent,
            parent_wire,
            dir,
        }
    }
    fn create_guardian(
        state: &SharedState,
        root: &CancellationToken,
        dir: &std::path::Path,
        peer: &PeerIdentity,
        recipe: &SpawnNativeAgent,
    ) -> (
        ClientId,
        phux_core::ids::SessionId,
        phux_core::ids::ResourceId,
        ResourceId,
    ) {
        let (owner, session, parent) = state.with_mut(|s| {
            s.set_server_socket_path(dir.join("isolated.sock"));
            let owner = s.new_client_id();
            s.set_peer_identity(owner, peer.clone());
            s.set_connection_grant(owner, ConnectionGrant::owner());
            let launch = phux_core::TerminalLaunch {
                env: vec![(
                    NATIVE_HOST_SPEC_ENV.into(),
                    serde_json::to_string(recipe).unwrap(),
                )],
                ..Default::default()
            };
            let (session, parent) = s
                .create_terminal(
                    crate::state::TerminalTarget::NewSession {
                        name: "alpha",
                        lifetime: phux_core::SessionLifetime::Persistent,
                    },
                    launch,
                    phux_core::ExitPolicy::Retain,
                    &s.actor_for(owner),
                )
                .unwrap();
            (owner, session, parent)
        });
        let terminal = crate::terminal_actor::TerminalActor::build_with_token_and_colors(
            80,
            24,
            None,
            phux_config::ScrollbackLimits::default(),
            root.child_token(),
            None,
        )
        .unwrap();
        let parent_wire = state.with_mut(|s| {
            let wire = s.spawn_resource_actor(
                parent,
                terminal.handle,
                root.child_token(),
                terminal.actor.run(),
            );
            s.activation_started(
                parent,
                Some(phux_core::ProcIdentity {
                    pid: i32::try_from(std::process::id()).unwrap(),
                    pgid: i32::try_from(std::process::id()).unwrap(),
                    start_raw: 1,
                    boot_id: [1; 16],
                }),
            );
            wire
        });
        (owner, session, parent, parent_wire)
    }

    fn create_agent(
        state: &SharedState,
        root: &CancellationToken,
        owner: ClientId,
        parent: phux_core::ids::ResourceId,
        parent_wire: &ResourceId,
        recipe: SpawnNativeAgent,
        credential: &str,
    ) -> ResourceId {
        state.with_mut(|s| {
            let core = s
                .create_agent_session(
                    parent,
                    &phux_core::AgentFacet {
                        provider: "omp".into(),
                        native_id: None,
                        state: None,
                    },
                    &s.actor_for(owner),
                )
                .unwrap();
            let token = root.child_token();
            let bundle = AgentSessionActor::build(parent, "omp", None, token.clone(), 1024 * 1024);
            let agent = s.spawn_resource_actor(core, bundle.handle, token, bundle.actor.run());
            s.native_agents_mut().bindings.insert(
                agent.clone(),
                NativeBinding {
                    launch: recipe,
                    agent: agent.clone(),
                    parent: parent_wire.clone(),
                    activation: "activation-A".into(),
                    proof: "private-test-proof".into(),
                    credential: credential.to_owned(),
                    host_pid: None,
                    generation: 0,
                    view: NativeView {
                        activation: "activation-A".into(),
                        backend: "omp-rpc".into(),
                        capabilities: NativeCapabilities::default(),
                        connected: false,
                        turn_id: None,
                        provider_session_id: None,
                        questions: vec![],
                    },
                    closed: false,
                    operations: BTreeMap::new(),
                    pending: VecDeque::new(),
                    answered: BTreeMap::new(),
                    publications: VecDeque::new(),
                    publication_high_water: 0,
                },
            );
            s.native_agents_mut()
                .wakes
                .insert(agent.clone(), Arc::new(Notify::new()));
            agent
        })
    }

    fn asked_inventory(&self) -> (Option<String>, bool, Option<Vec<u8>>) {
        self.state.with(|s| {
            (
                s.current_agent_asked(self.parent)
                    .map(|asked| asked.id.clone()),
                s.agent_is_asked(self.parent),
                s.metadata().get(
                    &Scope::Resource(self.parent_wire.clone()),
                    RESOURCE_ASKED_KEY,
                ),
            )
        })
    }

    fn completion(
        &self,
        generation: u64,
        operation_id: u64,
        disposition: NativeDisposition,
    ) -> NativeRequest {
        NativeRequest::Complete {
            agent_session_id: self.agent.clone(),
            activation: "activation-A".into(),
            binding_proof: "private-test-proof".into(),
            generation,
            result: NativeCommandResult {
                operation_id,
                disposition,
                detail: Some("adapter receipt".into()),
            },
        }
    }
    fn context(&self, client: ClientId) -> CommandContext {
        let (tx, _rx) = mpsc::channel(32);
        let mut ctx = CommandContext::for_test(&self.state, client, &tx);
        ctx.root_token = self.root.clone();
        ctx
    }
    async fn request(&self, client: ClientId, request: NativeRequest) -> CommandResult {
        let command = Command::NativeAgent {
            request: NativeRequestPayload::new(request.clone()).unwrap(),
        };
        assert!(matches!(
            self.state
                .with(|s| crate::policy::authorize_command(s, client, &command)),
            Ok(crate::policy::Admission::Run)
        ));
        handle(&self.context(client), request).await
    }
    async fn register(&self) -> u64 {
        decode::<serde_json::Value>(
            self.request(
                self.host,
                NativeRequest::Register {
                    agent_session_id: self.agent.clone(),
                    activation: "activation-A".into(),
                    binding_proof: "private-test-proof".into(),
                },
            )
            .await,
        )["generation"]
            .as_u64()
            .unwrap()
    }
    fn view() -> NativeView {
        NativeView {
            activation: "activation-A".into(),
            backend: "omp-rpc".into(),
            capabilities: NativeCapabilities {
                prompt: true.into(),
                interrupt: true.into(),
                questions: true.into(),
                follow_up: true.into(),
            },
            connected: true,
            turn_id: Some("turn-A".into()),
            provider_session_id: Some("provider-session-1".into()),
            questions: vec![],
        }
    }
    async fn publish(&self, generation: u64, id: u64, update: NativeUpdate) -> CommandResult {
        self.request(
            self.host,
            NativeRequest::Publish {
                agent_session_id: self.agent.clone(),
                activation: "activation-A".into(),
                binding_proof: "private-test-proof".into(),
                generation,
                publication_id: id,
                update,
            },
        )
        .await
    }
    async fn ready(&self) -> u64 {
        let generation = self.register().await;
        decode::<NativePublicationReceipt>(
            self.publish(generation, 1, NativeUpdate::View { view: Self::view() })
                .await,
        );
        generation
    }
    async fn command(
        &self,
        client: ClientId,
        id: u64,
        intent: NativeIntent,
    ) -> NativeCommandResult {
        decode(
            self.request(
                client,
                NativeRequest::Command {
                    agent_session_id: self.agent.clone(),
                    command: NativeCommand {
                        operation_id: id,
                        activation: "activation-A".into(),
                        intent,
                    },
                },
            )
            .await,
        )
    }
    async fn receive(&self, generation: u64) -> NativeCommand {
        decode(
            self.request(
                self.host,
                NativeRequest::Receive {
                    agent_session_id: self.agent.clone(),
                    activation: "activation-A".into(),
                    binding_proof: "private-test-proof".into(),
                    generation,
                },
            )
            .await,
        )
    }
    async fn cut(&self) -> crate::resource::agent_session::AgentSessionBootstrap {
        let bootstrap = self.state.with(|s| {
            s.terminal_from_wire(&self.agent)
                .and_then(|core| s.resource_handle(core))
                .unwrap()
                .agent_session()
                .unwrap()
                .bootstrap
                .clone()
        });
        let (reply, rx) = oneshot::channel();
        bootstrap
            .send(crate::resource::agent_session::BootstrapRequest {
                reply,
                after_seq: None,
                seal: None,
            })
            .await
            .unwrap();
        rx.await.unwrap()
    }
}
fn decode<T: serde::de::DeserializeOwned>(result: CommandResult) -> T {
    let CommandResult::OkWith(CommandValue::Json(text)) = result else {
        panic!("expected actual broker JSON receipt: {result:?}");
    };
    serde_json::from_str(&text).unwrap()
}
fn question(kind: &str) -> NativeQuestion {
    NativeQuestion {
        id: "question-A".into(),
        kind: kind.into(),
        title: "Provider question".into(),
        message: "Actual requested choice".into(),
        options: vec![NativeOption {
            id: "deny-once".into(),
            label: "Deny once".into(),
            description: None,
        }],
        questions: vec![],
        fields: serde_json::json!({}),
    }
}
fn prompt(text: &str) -> NativeIntent {
    NativeIntent::Prompt { text: text.into() }
}

#[tokio::test(flavor = "current_thread")]
async fn exact_resource_activation_and_operation_identity_are_required() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            fx.ready().await;
            let wrong_resource = fx
                .request(
                    fx.owner,
                    NativeRequest::Command {
                        agent_session_id: fx.parent_wire.clone(),
                        command: NativeCommand {
                            operation_id: 1,
                            activation: "activation-A".into(),
                            intent: prompt("do not execute"),
                        },
                    },
                )
                .await;
            assert_eq!(
                decode::<NativeCommandResult>(wrong_resource).disposition,
                NativeDisposition::Refused
            );
            let stale = fx
                .request(
                    fx.owner,
                    NativeRequest::Command {
                        agent_session_id: fx.agent.clone(),
                        command: NativeCommand {
                            operation_id: 99,
                            activation: "other-activation".into(),
                            intent: prompt("do not execute"),
                        },
                    },
                )
                .await;
            assert_eq!(
                decode::<NativeCommandResult>(stale).disposition,
                NativeDisposition::Refused
            );
            assert_eq!(
                fx.command(fx.owner, 99, prompt("changed activation and payload"))
                    .await
                    .disposition,
                NativeDisposition::Refused
            );
            assert_eq!(
                fx.command(fx.owner, 1, prompt("reserved input"))
                    .await
                    .disposition,
                NativeDisposition::Queued
            );
            assert_eq!(
                fx.command(fx.owner, 1, prompt("different input"))
                    .await
                    .disposition,
                NativeDisposition::Refused
            );
            assert_eq!(
                fx.command(fx.owner, 1, prompt("reserved input"))
                    .await
                    .disposition,
                NativeDisposition::Queued
            );
            assert_eq!(
                fx.command(
                    fx.owner,
                    2,
                    NativeIntent::Interrupt {
                        turn_id: "stale-turn".into()
                    }
                )
                .await
                .disposition,
                NativeDisposition::Refused
            );
            assert_eq!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].pending.len()),
                1
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn concurrent_viewers_get_exactly_one_question_responder() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    2,
                    NativeUpdate::QuestionOpened {
                        question: question("confirm"),
                    },
                )
                .await,
            );
            assert_eq!(
                fx.asked_inventory(),
                (Some("question-A".into()), true, Some(b"1".to_vec()))
            );
            // Codex nonblocking questions can outlive a completed model turn.
            // Only the provider's explicit question closure withdraws the request.
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    3,
                    NativeUpdate::State {
                        state: NativeState::Done,
                        turn_id: None,
                    },
                )
                .await,
            );
            assert_eq!(
                fx.asked_inventory(),
                (Some("question-A".into()), true, Some(b"1".to_vec()))
            );
            let second = fx.state.with_mut(|s| {
                let client = s.new_client_id();
                s.set_connection_grant(client, ConnectionGrant::owner());
                client
            });
            let answer = NativeIntent::Answer {
                question_id: "question-A".into(),
                response: serde_json::json!({"confirmed":false}),
            };
            let (a, b) = tokio::join!(
                fx.command(fx.owner, 10, answer.clone()),
                fx.command(second, 11, answer)
            );
            assert_eq!(
                [a.disposition, b.disposition]
                    .iter()
                    .filter(|disposition| **disposition == NativeDisposition::Queued)
                    .count(),
                1
            );
            assert_eq!(
                [a.disposition, b.disposition]
                    .iter()
                    .filter(|disposition| **disposition == NativeDisposition::Refused)
                    .count(),
                1
            );
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    4,
                    NativeUpdate::QuestionClosed {
                        id: "question-A".into(),
                        reason: "provider withdrew question".into(),
                    },
                )
                .await,
            );
            assert_eq!(fx.asked_inventory(), (None, false, None));
            assert_eq!(
                fx.command(
                    second,
                    12,
                    NativeIntent::Answer {
                        question_id: "question-A".into(),
                        response: serde_json::json!({"confirmed":true})
                    }
                )
                .await
                .disposition,
                NativeDisposition::Refused
            );
            fx.state.with_mut(|s| refuse_undeliverable(s, &fx.agent));
            assert!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].pending.is_empty())
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn human_permission_and_parent_input_lease_gate_native_commands() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    2,
                    NativeUpdate::QuestionOpened {
                        question: question("permission"),
                    },
                )
                .await,
            );
            assert_eq!(
                fx.command(
                    fx.host,
                    20,
                    NativeIntent::Answer {
                        question_id: "question-A".into(),
                        response: serde_json::json!({"option_id":"deny-once"})
                    }
                )
                .await
                .disposition,
                NativeDisposition::Refused
            );
            let other = fx.state.with_mut(|s| {
                let client = s.new_client_id();
                s.set_connection_grant(client, ConnectionGrant::owner());
                s.set_input_lease(fx.parent, client);
                client
            });
            assert_eq!(
                fx.command(fx.owner, 21, prompt("lease-blocked"))
                    .await
                    .disposition,
                NativeDisposition::Refused
            );
            fx.state.with_mut(|s| {
                s.release_input_lease(fx.parent, other);
            });
            assert_eq!(
                fx.command(
                    fx.owner,
                    22,
                    NativeIntent::Answer {
                        question_id: "question-A".into(),
                        response: serde_json::json!({"cancelled":true})
                    }
                )
                .await
                .disposition,
                NativeDisposition::Queued
            );
            let raw = super::super::commands::handle_route_input(
                &fx.state,
                fx.owner,
                &fx.parent_wire,
                phux_protocol::input::InputEvent::Paste(phux_protocol::input::paste::PasteEvent {
                    trust: phux_protocol::input::paste::PasteTrust::Trusted,
                    data: b"must not reach PTY".to_vec(),
                }),
            );
            assert!(matches!(
                raw,
                CommandResult::Error {
                    code: ErrorCode::InputNotWritten,
                    ..
                }
            ));
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn publication_dedupe_is_stamped_once_and_updates_lifecycle_inventory() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            let update = NativeUpdate::State {
                state: NativeState::Working,
                turn_id: Some("turn-A".into()),
            };
            let first: NativePublicationReceipt =
                decode(fx.publish(generation, 2, update.clone()).await);
            let replay: NativePublicationReceipt = decode(fx.publish(generation, 2, update).await);
            assert_eq!(first, replay);
            assert_eq!(fx.cut().await.base_seq, first.last_seq);
            assert!(matches!(
                fx.publish(
                    generation,
                    2,
                    NativeUpdate::State {
                        state: NativeState::Done,
                        turn_id: None
                    }
                )
                .await,
                CommandResult::Error { .. }
            ));
            let core = fx.state.with(|s| s.terminal_from_wire(&fx.agent).unwrap());
            assert_eq!(
                fx.state.with(|s| s
                    .registry()
                    .resource(core)
                    .unwrap()
                    .agent
                    .as_ref()
                    .unwrap()
                    .state
                    .clone()),
                Some("working".into())
            );
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    3,
                    NativeUpdate::QuestionOpened {
                        question: question("input"),
                    },
                )
                .await,
            );
            assert!(fx.state.with(|s| s.agent_is_asked(fx.parent)));
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    4,
                    NativeUpdate::QuestionClosed {
                        id: "question-A".into(),
                        reason: "provider cancelled".into(),
                    },
                )
                .await,
            );
            assert!(!fx.state.with(|s| s.agent_is_asked(fx.parent)));
            let forged = super::super::resource_commands::handle_append_resource_output(
                &fx.state,
                fx.owner,
                &fx.agent,
                Bytes::from_static(b"{\"type\":\"stop\"}\n"),
            )
            .await;
            assert!(matches!(
                forged,
                CommandResult::Error {
                    code: ErrorCode::NotProducer,
                    ..
                }
            ));
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn controller_binding_fences_old_generation_and_cannot_create_resources() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let old = fx.ready().await;
            let new = fx.register().await;
            assert!(new > old);
            assert!(matches!(
                fx.publish(
                    old,
                    2,
                    NativeUpdate::Notice {
                        level: "info".into(),
                        text: "old writer".into()
                    }
                )
                .await,
                CommandResult::Error {
                    code: ErrorCode::PreconditionFailed,
                    ..
                }
            ));
            let wrong_proof = fx
                .request(
                    fx.host,
                    NativeRequest::Register {
                        agent_session_id: fx.agent.clone(),
                        activation: "activation-A".into(),
                        binding_proof: "another-host".into(),
                    },
                )
                .await;
            assert!(matches!(
                wrong_proof,
                CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    ..
                }
            ));
            let spawn = Command::NativeAgent {
                request: NativeRequestPayload::new(NativeRequest::Spawn {
                    spec: fx
                        .state
                        .with(|s| s.native_agents().bindings[&fx.agent].launch.clone()),
                })
                .unwrap(),
            };
            assert!(
                fx.state
                    .with(|s| crate::policy::authorize_command(s, fx.host, &spawn))
                    .is_err()
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn dispatched_upgrade_operation_is_unknown_and_never_automatically_replayed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            assert_eq!(
                fx.command(fx.owner, 31, prompt("may already have reached SDK"))
                    .await
                    .disposition,
                NativeDisposition::Queued
            );
            assert_eq!(fx.receive(generation).await.operation_id, 31);
            let cut = fx.cut().await;
            let mut blob = fx.state.with_mut(|s| {
                s.assemble_upgrade_blob(7, &crate::state::UpgradeHandoffs::default())
            });
            let binding = fx
                .state
                .with(|s| s.native_agents().bindings[&fx.agent].clone());
            blob.native_agents = vec![binding];
            blob.agent_sessions = vec![crate::upgrade::blob::AgentSessionBlob {
                wire_id: fx.agent.local_id().unwrap(),
                parent_wire_id: fx.parent_wire.local_id().unwrap(),
                provider: "omp".into(),
                native_id: None,
                state: None,
                base_seq: cut.base_seq,
                dropped: cut.dropped,
                ended: false,
                records: cut
                    .records
                    .iter()
                    .map(|record| String::from_utf8(record.to_vec()).unwrap())
                    .collect(),
                key: None,
                native_publications: cut.native_publications,
                native_publication_high_water: cut.native_publication_high_water,
            }];
            fx.state.with_mut(|s| s.restore_native_agents(&blob));
            let restored = fx
                .state
                .with(|s| s.native_agents().bindings[&fx.agent].clone());
            assert_eq!(
                restored.operations[&31].result.disposition,
                NativeDisposition::Unknown
            );
            assert!(restored.pending.is_empty());
            assert_eq!(restored.agent, fx.agent);
            assert_eq!(restored.activation, "activation-A");
            assert_eq!(
                restored.view.provider_session_id.as_deref(),
                Some("provider-session-1")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn lost_publication_ack_reuses_actor_receipt_without_duplicate_records() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            let update = NativeUpdate::State {
                state: NativeState::Blocked,
                turn_id: Some("turn-A".into()),
            };
            let accepted: NativePublicationReceipt =
                decode(fx.publish(generation, 2, update.clone()).await);
            // Simulate cancellation after the actor's atomic acceptance but
            // before the broker/client sees its receipt.
            fx.state.with_mut(|s| {
                let binding = s.native_agents_mut().bindings.get_mut(&fx.agent).unwrap();
                let view = binding.view.clone();
                let publication = binding.publications.back_mut().unwrap();
                publication.receipt = None;
                publication.view = Some(view);
            });
            let retried: NativePublicationReceipt = decode(fx.publish(generation, 2, update).await);
            assert_eq!(retried, accepted);
            let cut = fx.cut().await;
            assert_eq!(cut.base_seq, accepted.last_seq);
            assert_eq!(cut.native_publications.len(), 2);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn upgrade_recovers_atomic_publication_receipt_without_retained_record() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            let update = NativeUpdate::State {
                state: NativeState::Done,
                turn_id: None,
            };
            let accepted: NativePublicationReceipt =
                decode(fx.publish(generation, 2, update.clone()).await);
            fx.state.with_mut(|s| {
                let binding = s.native_agents_mut().bindings.get_mut(&fx.agent).unwrap();
                let view = binding.view.clone();
                let publication = binding.publications.back_mut().unwrap();
                publication.receipt = None;
                publication.view = Some(view);
            });
            let cut = fx.cut().await;
            let mut blob = fx.state.with(|s| {
                s.with_upgrade_metadata(
                    s.assemble_upgrade_blob(7, &crate::state::UpgradeHandoffs::default()),
                )
            });
            blob.agent_sessions = vec![crate::upgrade::blob::AgentSessionBlob {
                wire_id: fx.agent.local_id().unwrap(),
                parent_wire_id: fx.parent_wire.local_id().unwrap(),
                provider: "omp".into(),
                native_id: Some("provider-session-1".into()),
                state: Some("done".into()),
                base_seq: cut.base_seq,
                dropped: cut.dropped,
                ended: false,
                records: vec![],
                key: None,
                native_publications: cut.native_publications,
                native_publication_high_water: cut.native_publication_high_water,
            }];
            fx.state.with_mut(|s| s.restore_native_agents(&blob));
            let next = fx.register().await;
            let recovered: NativePublicationReceipt = decode(fx.publish(next, 2, update).await);
            assert_eq!(accepted, recovered);
            assert_eq!(fx.cut().await.base_seq, accepted.last_seq);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_is_not_admission_and_eventual_receipts_are_truthful() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            assert_eq!(
                fx.command(fx.owner, 40, prompt("reserved input"))
                    .await
                    .disposition,
                NativeDisposition::Queued
            );
            fx.receive(generation).await;
            assert_eq!(
                fx.command(fx.owner, 40, prompt("reserved input"))
                    .await
                    .disposition,
                NativeDisposition::Unknown
            );
            for (disposition, detail) in [
                (
                    NativeDisposition::Queued,
                    "SDK request submitted; provider consumption unconfirmed",
                ),
                (NativeDisposition::Completed, "actual adapter completion"),
            ] {
                let receipt = NativeCommandResult {
                    operation_id: 40,
                    disposition,
                    detail: Some(detail.into()),
                };
                assert_eq!(
                    fx.request(
                        fx.host,
                        NativeRequest::Complete {
                            agent_session_id: fx.agent.clone(),
                            activation: "activation-A".into(),
                            binding_proof: "private-test-proof".into(),
                            generation,
                            result: receipt.clone(),
                        }
                    )
                    .await,
                    CommandResult::Ok
                );
                assert_eq!(
                    fx.command(fx.owner, 40, prompt("reserved input")).await,
                    receipt
                );
            }
            assert!(matches!(
                fx.request(
                    fx.host,
                    NativeRequest::Complete {
                        agent_session_id: fx.agent.clone(),
                        activation: "activation-A".into(),
                        binding_proof: "private-test-proof".into(),
                        generation,
                        result: NativeCommandResult {
                            operation_id: 40,
                            disposition: NativeDisposition::Admitted,
                            detail: None
                        },
                    }
                )
                .await,
                CommandResult::Error { .. }
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoked_queued_caller_is_refused_before_host_delivery_and_closed_source_stays_readable() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            fx.ready().await;
            assert_eq!(
                fx.command(fx.owner, 50, prompt("reserved input"))
                    .await
                    .disposition,
                NativeDisposition::Queued
            );
            fx.state.with_mut(|s| {
                s.remove_peer_identity(fx.owner);
                refuse_undeliverable(s, &fx.agent);
            });
            assert!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].pending.is_empty())
            );
            assert_eq!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].operations[&50]
                        .result
                        .disposition),
                NativeDisposition::Refused
            );
            let before = fx.cut().await.base_seq;
            source_exited(&fx.state, fx.parent).await;
            let cut = fx.cut().await;
            assert!(cut.base_seq > before);
            assert!(
                !fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].view.connected)
            );
            assert!(
                cut.records
                    .iter()
                    .any(|record| String::from_utf8_lossy(record).contains("\"connected\":false"))
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_identity_is_durable_without_relaunch_or_lifetime_mutation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let before = fx
                .state
                .with(|s| s.registry().record(fx.parent).unwrap().clone());
            let generation = fx.ready().await;
            let after = fx
                .state
                .with(|s| s.registry().record(fx.parent).unwrap().clone());
            assert_eq!(before.activation, after.activation);
            assert_eq!(before.session, after.session);
            assert_eq!(before.on_exit, after.on_exit);
            let Some(phux_core::LaunchSpec::Terminal(launch)) = after.launch else {
                panic!("native guardian recipe");
            };
            let encoded = &launch
                .env
                .iter()
                .find(|(key, _)| key == NATIVE_HOST_SPEC_ENV)
                .unwrap()
                .1;
            let saved: SpawnNativeAgent = serde_json::from_str(encoded).unwrap();
            assert_eq!(saved.native_id.as_deref(), Some("provider-session-1"));
            assert!(
                saved.guardian.is_none()
                    && saved.guardian_key.is_none()
                    && saved.guardian_expected.is_none()
            );
            assert!(saved.env.is_empty());
            assert!(saved.sdk_runtime.is_none() && saved.node_bridge.is_none());
            let mut forged = Fixture::view();
            forged.provider_session_id = Some("not-owner-session".into());
            assert!(matches!(
                fx.request(
                    fx.host,
                    NativeRequest::Publish {
                        agent_session_id: fx.agent.clone(),
                        activation: "activation-A".into(),
                        binding_proof: "wrong-owner".into(),
                        generation,
                        publication_id: 2,
                        update: NativeUpdate::View { view: forged },
                    }
                )
                .await,
                CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    ..
                }
            ));
            assert_eq!(
                fx.state
                    .with(|s| s.registry().record(fx.parent).unwrap().revision),
                after.revision
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn sealed_cut_refuses_new_input_and_preserves_same_publication_retry() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            let before = fx.cut().await.base_seq;
            fx.state.with_mut(|s| s.native_agents_mut().sealed = true);
            assert_eq!(
                fx.command(fx.owner, 77, prompt("must not dispatch after cut"))
                    .await
                    .disposition,
                NativeDisposition::Refused
            );
            let update = NativeUpdate::Notice {
                level: "info".into(),
                text: "actual SDK output retained during cut".into(),
            };
            assert!(matches!(
                fx.publish(generation, 2, update.clone()).await,
                CommandResult::Error {
                    code: ErrorCode::Overflow,
                    ..
                }
            ));
            assert_eq!(fx.cut().await.base_seq, before);
            assert!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].pending.is_empty())
            );
            fx.state.with_mut(|s| s.native_agents_mut().sealed = false);
            let receipt: NativePublicationReceipt = decode(fx.publish(generation, 2, update).await);
            assert!(receipt.first_seq > before);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn generic_hooks_cannot_create_another_source_or_self_input_under_native_guardian() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            fx.ready().await;
            assert_eq!(
                fx.command(fx.host, 90, prompt("tool must not control its own model"))
                    .await
                    .disposition,
                NativeDisposition::Refused
            );
            // The durable, explicit recipe guards ownership even without the
            // live broker map (for example an old reporting extension at restore).
            fx.state.with_mut(|s| {
                s.native_agents_mut().bindings.remove(&fx.agent);
            });
            let (tx, mut rx) = mpsc::channel(4);
            super::super::resource_commands::spawn_agent_session(
                &fx.state,
                fx.owner,
                900,
                &phux_protocol::wire::frame::SpawnResource::agent_session(
                    fx.parent_wire.clone(),
                    "legacy-reporting-hook",
                ),
                None,
                &tx,
                &fx.root,
                fx.context(fx.owner).limits,
                &fx.root,
            )
            .await;
            assert!(matches!(
                rx.recv().await,
                Some(crate::mailbox::Outbound::Frame(
                    FrameKind::ResourceSpawned {
                        request_id: 900,
                        result: phux_protocol::wire::frame::SpawnResult::Err(
                            phux_protocol::wire::frame::SpawnError::SpawnFailed(_)
                        )
                    }
                ))
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_declared_custom_answer_does_not_reserve_the_question() {
    tokio::task::LocalSet::new().run_until(async {
        let fx = Fixture::new(); let generation = fx.ready().await;
        let mut ask = question("ask");
        ask.fields = serde_json::json!({"allow_custom_input":false});
        ask.questions.push(NativeQuestionItem { id: "choice".into(), question: "Choose".into(), header: "Choice".into(),
            options: ask.options.clone(), multi: false, recommended: None });
        decode::<NativePublicationReceipt>(fx.publish(generation, 2, NativeUpdate::QuestionOpened { question: ask }).await);
        let invalid = NativeIntent::Answer { question_id: "question-A".into(),
            response: serde_json::json!({"answers":[{"id":"choice","selected_options":[],"custom_input":"unsupported"}]}) };
        assert_eq!(fx.command(fx.owner, 100, invalid.clone()).await.disposition, NativeDisposition::Refused);
        let corrected = NativeIntent::Answer { question_id: "question-A".into(),
            response: serde_json::json!({"answers":[{"id":"choice","selected_options":["deny-once"]}]}) };
        assert_eq!(fx.command(fx.owner, 101, corrected.clone()).await.disposition, NativeDisposition::Queued);
        assert_eq!(fx.command(fx.owner, 100, invalid).await.disposition, NativeDisposition::Refused);
        assert_eq!(fx.receive(generation).await.intent, corrected);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_answer_refusal_releases_only_its_exact_live_reservation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for case in [
                "local",
                "unknown",
                "queued",
                "admitted",
                "completed",
                "other-operation",
                "other-activation",
                "closed-question",
                "submitted-then-unknown",
            ] {
                let fx = Fixture::new();
                let generation = fx.ready().await;
                decode::<NativePublicationReceipt>(
                    fx.publish(
                        generation,
                        2,
                        NativeUpdate::QuestionOpened {
                            question: question("form"),
                        },
                    )
                    .await,
                );
                let answer = NativeIntent::Answer {
                    question_id: "question-A".into(),
                    response: serde_json::json!({"values":{"count":"1"}}),
                };
                assert_eq!(
                    fx.command(fx.owner, 110, answer.clone()).await.disposition,
                    NativeDisposition::Queued
                );
                fx.receive(generation).await;
                let complete = |disposition| fx.completion(generation, 110, disposition);
                prepare_answer_reservation_case(&fx, generation, case).await;
                if case != "unknown" {
                    let receipt = fx
                        .request(fx.host, complete(NativeDisposition::Refused))
                        .await;
                    if case == "completed" {
                        assert!(matches!(receipt, CommandResult::Error { .. }));
                    } else {
                        assert_eq!(receipt, CommandResult::Ok);
                    }
                }
                let corrected = NativeIntent::Answer {
                    question_id: "question-A".into(),
                    response: serde_json::json!({"values":{"count":1}}),
                };
                let result = fx.command(fx.owner, 111, corrected).await;
                assert_eq!(
                    result.disposition,
                    if case == "local" {
                        NativeDisposition::Queued
                    } else {
                        NativeDisposition::Refused
                    },
                    "{case}"
                );
                if case == "local" {
                    // Replaying the old final refusal cannot remove the new responder.
                    assert_eq!(
                        fx.request(fx.host, complete(NativeDisposition::Refused))
                            .await,
                        CommandResult::Ok
                    );
                    assert_eq!(
                        fx.command(fx.owner, 110, answer).await.disposition,
                        NativeDisposition::Refused
                    );
                    assert_eq!(
                        fx.command(
                            fx.owner,
                            112,
                            NativeIntent::Answer {
                                question_id: "question-A".into(),
                                response: serde_json::json!({"cancelled":true})
                            }
                        )
                        .await
                        .disposition,
                        NativeDisposition::Refused
                    );
                    assert_eq!(
                        fx.state
                            .with(|s| s.native_agents().bindings[&fx.agent].answered["question-A"]),
                        111
                    );
                } else {
                    assert!(fx.state.with(|s| {
                        s.native_agents().bindings[&fx.agent]
                            .answered
                            .contains_key("question-A")
                    }));
                }
            }
        })
        .await;
}

async fn prepare_answer_reservation_case(fx: &Fixture, generation: u64, case: &str) {
    let complete = |disposition| fx.completion(generation, 110, disposition);
    match case {
        "queued" => assert_eq!(
            fx.request(fx.host, complete(NativeDisposition::Queued))
                .await,
            CommandResult::Ok
        ),
        "admitted" => assert_eq!(
            fx.request(fx.host, complete(NativeDisposition::Admitted))
                .await,
            CommandResult::Ok
        ),
        "completed" => assert_eq!(
            fx.request(fx.host, complete(NativeDisposition::Completed))
                .await,
            CommandResult::Ok
        ),
        "submitted-then-unknown" => {
            assert_eq!(
                fx.request(fx.host, complete(NativeDisposition::Queued))
                    .await,
                CommandResult::Ok
            );
            assert_eq!(
                fx.request(fx.host, complete(NativeDisposition::Unknown))
                    .await,
                CommandResult::Ok
            );
        }
        "other-operation" => fx.state.with_mut(|s| {
            s.native_agents_mut()
                .bindings
                .get_mut(&fx.agent)
                .unwrap()
                .answered
                .insert("question-A".into(), 999);
        }),
        "other-activation" => fx.state.with_mut(|s| {
            s.native_agents_mut()
                .bindings
                .get_mut(&fx.agent)
                .unwrap()
                .operations
                .get_mut(&110)
                .unwrap()
                .command
                .activation = "older-activation".into();
        }),
        "closed-question" => {
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    3,
                    NativeUpdate::QuestionClosed {
                        id: "question-A".into(),
                        reason: "withdrawn".into(),
                    },
                )
                .await,
            );
        }
        "local" | "unknown" => {}
        _ => panic!("unhandled reservation case: {case}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn viewer_native_spawn_cannot_acquire_input_authority() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use std::os::unix::fs::PermissionsExt;
            let fx = Fixture::new();
            // A real inert guardian process, not a provider or protocol echo.
            let executable = fx.dir.path().join("guardian");
            std::fs::write(&executable, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let (tx, _rx) = mpsc::channel(32);
            let spec = fx.state.with_mut(|s| {
                s.attach_default_caps(fx.owner, "alpha", tx).unwrap();
                s.set_attached_viewer(fx.owner, true);
                s.set_native_host_executable(Some(executable));
                s.native_agents().bindings[&fx.agent].launch.clone()
            });
            let spawned: SpawnedNativeAgent =
                decode(fx.request(fx.owner, NativeRequest::Spawn { spec }).await);
            assert!(fx.state.with(|s| s.is_viewer(fx.owner, &spawned.parent)));
            assert!(fx.state.with(|s| s.is_viewer(fx.owner, &spawned.agent)));
            fx.state.with_mut(|s| {
                let binding = s
                    .native_agents_mut()
                    .bindings
                    .get_mut(&spawned.agent)
                    .unwrap();
                binding.view.connected = true;
                binding.view.capabilities.prompt = true.into();
            });
            let request = NativeRequest::Command {
                agent_session_id: spawned.agent.clone(),
                command: NativeCommand {
                    operation_id: 120,
                    activation: spawned.activation,
                    intent: prompt("viewer must not send"),
                },
            };
            let command = Command::NativeAgent {
                request: NativeRequestPayload::new(request.clone()).unwrap(),
            };
            assert!(
                fx.state
                    .with(|s| crate::policy::authorize_command(s, fx.owner, &command))
                    .is_err(),
                "viewer input must be denied by the public dispatch guard"
            );
            let result: NativeCommandResult = decode(handle(&fx.context(fx.owner), request).await);
            assert_eq!(result.disposition, NativeDisposition::Refused);
            assert!(fx.state.with(|s| {
                s.native_agents().bindings[&spawned.agent]
                    .pending
                    .is_empty()
            }));
            let parent = fx
                .state
                .with(|s| s.terminal_from_wire(&spawned.parent).unwrap());
            fx.state.with_mut(|s| {
                s.reap_terminal(parent);
            });
        })
        .await;
}
