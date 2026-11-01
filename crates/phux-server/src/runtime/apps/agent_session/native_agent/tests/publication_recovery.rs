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
            let core = fx.state.with(|s| s.resource_from_wire(&fx.agent).unwrap());
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
            let forged = crate::runtime::apps::agent_session::append::handle_append_resource_output(
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
