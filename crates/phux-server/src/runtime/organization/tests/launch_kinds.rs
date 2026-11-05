use super::*;

fn projection_specs() -> Vec<phux_core::LaunchSpec> {
    vec![
        phux_core::LaunchSpec::Agent(phux_core::AgentLaunch {
            provider: "test-producer".to_owned(),
            native_id: None,
        }),
        phux_core::LaunchSpec::Tree(phux_core::resource::TreeLaunch {
            target: "test-app".to_owned(),
        }),
        phux_core::LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
            target: "test-window".to_owned(),
        }),
    ]
}

fn inactive_projection(state: &SharedState, spec: phux_core::LaunchSpec) -> phux_core::Resource {
    state.with_mut(|s| {
        let (_, parent) = s.seed_session("work");
        let id = s
            .create_document(parent, spec, &phux_core::Actor::owner())
            .unwrap();
        let record = s.registry().record(id).unwrap().clone();
        s.commit_system(phux_core::Op::ActivationEnded {
            resource: record.key,
            activation: record.activation.as_ref().unwrap().id,
            state: phux_core::ExecState::Completed,
            outcome: phux_core::Outcome::Unobserved,
        })
        .unwrap();
        s.org().resource(&record.key).unwrap().clone()
    })
}

#[test]
fn reconciled_projections_cannot_relaunch_as_terminals() {
    run(async {
        for spec in projection_specs() {
            let state = SharedState::new();
            let before = inactive_projection(&state, spec);
            let resource = ow::resource_key(before.key).unwrap();
            let result = handle(
                &state,
                CLIENT,
                Command::ResourceRelaunch {
                    operation: op(71),
                    resource,
                    expected: Revision::new(before.revision.get()),
                },
                &CancellationToken::new(),
            );
            assert!(
                matches!(
                    result,
                    CommandResult::Refused(WireRefusal::Unsupported { .. })
                ),
                "{result:?}"
            );
            state.with(|s| {
                assert_eq!(s.org().resource(&before.key), Some(&before));
                let id = s.registry().resource_id(&before.key).unwrap();
                assert!(
                    s.resource_handle(id).is_none(),
                    "no wrong-kind engine is installed"
                );
                assert!(
                    s.org().receipt(&ow::core_operation(&op(71))).is_none(),
                    "no accepted operation is written"
                );
            });
        }
    });
}

#[test]
fn projection_relaunch_preserves_stale_revision_and_execution_refusals() {
    run(async {
        let state = SharedState::new();
        let before = inactive_projection(&state, projection_specs().remove(0));
        let resource = ow::resource_key(before.key).unwrap();
        let conflict = handle(
            &state,
            CLIENT,
            Command::ResourceRelaunch {
                operation: op(72),
                resource,
                expected: Revision::new(0),
            },
            &CancellationToken::new(),
        );
        assert!(
            matches!(
                conflict,
                CommandResult::Refused(WireRefusal::Conflict { .. })
            ),
            "{conflict:?}"
        );

        let running = SharedState::new();
        let (key, revision) = running.with_mut(|s| {
            let (_, parent) = s.seed_session("work");
            let id = s
                .create_document(
                    parent,
                    projection_specs().remove(1),
                    &phux_core::Actor::owner(),
                )
                .unwrap();
            let record = s.registry().record(id).unwrap();
            (
                ow::resource_key(record.key).unwrap(),
                Revision::new(record.revision.get()),
            )
        });
        let current = handle(
            &running,
            CLIENT,
            Command::ResourceRelaunch {
                operation: op(73),
                resource: key,
                expected: revision,
            },
            &CancellationToken::new(),
        );
        assert!(
            matches!(
                current,
                CommandResult::Refused(WireRefusal::InvalidArgument { .. })
            ),
            "{current:?}"
        );
    });
}

#[test]
fn failed_effect_settles_only_its_own_activation() {
    run(async {
        let state = SharedState::new();
        let before = inactive_projection(&state, projection_specs().remove(2));
        let commit = state.with_mut(|s| {
            s.commit(
                phux_core::Op::ResourceRelaunch {
                    resource: before.key,
                    expected: before.revision,
                    launch: None,
                },
                &phux_core::Actor::owner(),
            )
            .unwrap()
        });
        let effect = commit.applied.effects.into_iter().next().unwrap();
        super::super::effects::run_effect(&state, effect, &CancellationToken::new());
        let failed = state.with(|s| s.org().resource(&before.key).unwrap().clone());
        assert_eq!(
            failed.activation.as_ref().unwrap().state,
            phux_core::ExecState::Failed
        );
        assert!(state.with(|s| {
            s.resource_handle(s.registry().resource_id(&before.key).unwrap())
                .is_none()
        }));

        // The old effect cannot fail or start the next activation.
        let next = state.with_mut(|s| {
            s.commit(
                phux_core::Op::ResourceRelaunch {
                    resource: before.key,
                    expected: failed.revision,
                    launch: None,
                },
                &phux_core::Actor::owner(),
            )
            .unwrap()
        });
        let before_stale = state.with(|s| s.org().resource(&before.key).unwrap().clone());
        let old = phux_core::Effect::Launch {
            resource: before.key,
            activation: failed.activation.unwrap().id,
            spec: before.launch.unwrap(),
        };
        super::super::effects::run_effect(&state, old, &CancellationToken::new());
        assert_eq!(
            state.with(|s| s.org().resource(&before.key).unwrap().clone()),
            before_stale
        );
        assert_eq!(next.applied.effects.len(), 1);
    });
}

#[test]
fn existing_launch_receipt_replays_even_when_the_kind_no_longer_supports_launch() {
    run(async {
        let state = SharedState::new();
        let before = inactive_projection(&state, projection_specs().remove(0));
        let operation = op(74);
        let commit = state.with_mut(|s| {
            s.commit_command_from(
                &phux_core::Command {
                    operation: ow::core_operation(&operation),
                    op: phux_core::Op::ResourceRelaunch {
                        resource: before.key,
                        expected: before.revision,
                        launch: None,
                    },
                },
                &phux_core::Actor::owner(),
                Some(CLIENT),
            )
            .unwrap()
        });
        super::super::effects::run_effect(
            &state,
            commit.applied.effects.into_iter().next().unwrap(),
            &CancellationToken::new(),
        );
        let after_failure = state.with(|s| s.org().resource(&before.key).unwrap().clone());
        let result = handle(
            &state,
            CLIENT,
            Command::ResourceRelaunch {
                operation,
                resource: ow::resource_key(before.key).unwrap(),
                expected: Revision::new(before.revision.get()),
            },
            &CancellationToken::new(),
        );
        assert!(
            matches!(result, CommandResult::Accepted { operation: replay } if replay == operation),
            "{result:?}"
        );
        assert_eq!(
            state.with(|s| s.org().resource(&before.key).unwrap().clone()),
            after_failure
        );
        assert!(state.with(|s| {
            s.resource_handle(s.registry().resource_id(&before.key).unwrap())
                .is_none()
        }));
    });
}
