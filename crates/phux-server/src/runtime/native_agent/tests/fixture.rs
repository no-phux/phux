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
            backend: "acp".into(),
            provider: "omp".into(),
            command: vec!["provider-not-started-in-broker-test".into()],
            cwd: None,
            env: BTreeMap::new(),
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
        let terminal = crate::runtime::apps::terminal::test_support::terminal_under(80, 24, root.child_token());
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
                        backend: "acp".into(),
                        capabilities: NativeCapabilities::default(),
                        connected: false,
                        turn_id: None,
                        provider_session_id: None,
                        questions: vec![],
                    },
                    closed: false,
                    closure_committed: false,
                    publication_committed: 0,
                    evidence_seq: 0,
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
            backend: "acp".into(),
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
            s.resource_from_wire(&self.agent)
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
