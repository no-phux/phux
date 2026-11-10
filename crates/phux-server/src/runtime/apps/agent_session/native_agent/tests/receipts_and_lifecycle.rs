use super::*;

#[tokio::test(flavor = "current_thread")]
async fn routed_native_relaunch_retry_replays_without_rebinding_or_reexecuting() {
    tokio::task::LocalSet::new().run_until(async {
        let fx = Fixture::new();
        let operation = phux_protocol::ids::OperationId::new([97; 16]).unwrap();
        let (guardian, resource, expected) = fx.state.with_mut(|s| {
            let record = s.registry().record(fx.parent).unwrap().clone();
            let phux_core::LaunchSpec::Terminal(launch) = record.launch.unwrap() else {
                panic!("native guardian has a Terminal recipe");
            };
            let (_, guardian) = s.create_terminal(
                crate::state::TerminalTarget::Session(s.registry().session_of(fx.parent).unwrap()), launch.clone(),
                phux_core::ExitPolicy::Retain, &s.actor_for(fx.owner),
            ).unwrap();
            s.activation_ended(guardian, phux_core::ExitOutcome::exited(0), false);
            let record = s.registry().record(guardian).unwrap().clone();
            s.relaunch_native_guardian(
                guardian, &launch, fx.owner,
                Some(crate::org_wire::core_operation(&operation)), Some(record.revision),
            ).unwrap();
            (guardian, crate::org_wire::resource_key(record.key).unwrap(), crate::org_wire::revision(record.revision))
        });
        let before = fx.state.with(|s| s.registry().record(guardian).unwrap().clone());
        let (tx, mut rx) = mpsc::channel(4);
        let ctx = crate::runtime::commands::CommandContext::for_test(&fx.state, fx.owner, &tx);
        for (request_id, revision) in [(1, expected), (2, phux_protocol::ids::Revision::new(0))] {
            crate::runtime::commands::handle_command(&ctx, request_id, phux_protocol::wire::frame::Command::ResourceRelaunch {
                operation, resource, expected: revision,
            }).await;
            let Some(crate::state::Outbound::Frame(phux_protocol::wire::frame::FrameKind::CommandResult { result, .. })) = rx.recv().await else {
                panic!("correlated relaunch result");
            };
            if request_id == 1 {
                assert!(matches!(result, CommandResult::Accepted { operation: replay } if replay == operation), "{result:?}");
            } else {
                assert!(matches!(result, CommandResult::Refused(phux_protocol::wire::frame::Refusal::DuplicateOperation)), "{result:?}");
            }
        }
        assert_eq!(fx.state.with(|s| s.registry().record(guardian).unwrap().clone()), before);
        assert!(fx.state.with(|s| s.resource_handle(guardian).is_none()));
    }).await;
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
            let resource = phux_protocol::wire::frame::SpawnResource::agent_session(
                fx.parent_wire.clone(),
                "legacy-reporting-hook",
            );
            crate::runtime::apps::agent_session::spawn::spawn_agent_session(
                crate::runtime::apps::ResourceSpawn {
                    state: &fx.state,
                    client_id: fx.owner,
                    request_id: 900,
                    resource: &resource,
                    satellite: None,
                    launch: crate::runtime::apps::spawn::SpawnLaunch {
                        group: crate::state::DEFAULT_GROUP_ID,
                        command: None,
                        cwd: None,
                        env: None,
                        term: None,
                        owner_terminal: None,
                        agent_session: None,
                        initial_size: None,
                    },
                    out_tx: &tx,
                    root_token: &fx.root,
                    profile: phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
                    limits: fx.context(fx.owner).limits,
                    connection_token: &fx.root,
                    output_pumps: &mut tokio::task::JoinSet::new(),
                    defer_subscription: false,
                    decodes: phux_protocol::caps::ResourceKindSet::LEGACY,
                },
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
