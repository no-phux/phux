use super::*;

#[test]
fn mismatched_registration_preserves_the_existing_engine_and_wire_identity() {
    let mut state = ServerState::new();
    let (_, parent) = state.seed_session("work");
    let core = state
        .create_document(
            parent,
            phux_core::LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                target: "test-app".into(),
            }),
            &phux_core::Actor::owner(),
        )
        .unwrap();
    let token = CancellationToken::new();
    let bundle = phux_tree::TreeActor::build(parent, "test-app", token.clone());
    let wire = state
        .register_resource_handle(core, bundle.handle.clone(), token.clone())
        .unwrap();
    let replacement = CancellationToken::new();
    let error = state
        .register_resource_handle(core, crate::state::tests::mk_handle(), replacement.clone())
        .unwrap_err();
    assert_eq!(
        error,
        ResourceRegistrationError::WrongKind {
            resource: core,
            expected: ResourceKind::AccessibilityTree,
            actual: ResourceKind::Terminal,
        }
    );
    let retained = state.resource_handle(core).unwrap();
    assert_eq!(retained.kind, ResourceKind::AccessibilityTree);
    assert!(retained.output.same_channel(&bundle.handle.output));
    assert_eq!(state.resource_wire_of(core), Some(wire));
    assert!(!token.is_cancelled());
    assert!(!replacement.is_cancelled());
}

#[test]
fn refused_actor_registration_neither_allocates_a_wire_nor_spawns() {
    // No LocalSet: spawning either rejected future would panic here.
    let mut state = ServerState::new();
    let (_, parent) = state.seed_session("work");
    let core = state
        .create_document(
            parent,
            phux_core::LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
                target: "test-window".into(),
            }),
            &phux_core::Actor::owner(),
        )
        .unwrap();
    assert_eq!(state.resource_wire_of(core), None);
    let error = state
        .spawn_resource_actor(
            core,
            crate::state::tests::mk_handle(),
            CancellationToken::new(),
            async { panic!("refused actor must not run") },
        )
        .unwrap_err();
    assert!(matches!(error, ResourceRegistrationError::WrongKind { .. }));
    assert_eq!(state.resource_wire_of(core), None);
    assert!(state.resource_handle(core).is_none());

    state.retire_record(core, phux_core::ExitOutcome::UNKNOWN);
    let absent = state
        .spawn_resource_actor(
            core,
            crate::state::tests::mk_handle(),
            CancellationToken::new(),
            async { panic!("absent record must not run") },
        )
        .unwrap_err();
    assert_eq!(absent, ResourceRegistrationError::NotFound(core));
    assert_eq!(state.resource_wire_of(core), None);
    assert!(state.resource_handle(core).is_none());
}
