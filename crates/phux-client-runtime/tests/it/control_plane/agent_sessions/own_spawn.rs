//! Agent-parent admission across foreign spawns and reconnect replay.
use super::*;

fn followed_own_spawn() -> ControlPlane {
    let (mut plane, attach_id) = subscribing(true);
    let mut before_spawn = beta_snapshot();
    before_spawn
        .resources
        .retain(|resource| resource.id != terminal() && resource.id != ResourceId::local(AGENT));
    // A foreign spawn needs an existing member to name its destination.
    // @7 is the not-yet-created parent, so route through a separate member.
    let owner = ResourceId::local(6);
    before_spawn
        .resources
        .push(ResourceInfo::new(owner.clone(), WindowId::new(1), 20, 4));
    attach_to(&mut plane, attach_id, before_spawn);
    ready(&mut plane, attach_id, ResourceId::local(8));
    let request_id = plane.spawn_terminal(SpawnRequest {
        session_id: Some(1),
        ..SpawnRequest::default()
    });
    assert!(
        plane.take_outbound().iter().any(|bytes| matches!(
            decode(bytes),
            FrameKind::SpawnResource { request_id: id, owner_terminal: Some(target), .. }
                if id == request_id && target == owner
        )),
        "the foreign spawn must be submitted before its receipt is fed"
    );
    plane
        .feed(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(terminal()),
        })
        .unwrap();
    let request_id = plane.refresh_topology().unwrap();
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(beta_snapshot())),
        })
        .unwrap();
    let subscriptions = attach_resources(&mut plane);
    let request_id = subscriptions
        .iter()
        .find(|(_, id)| *id == ResourceId::local(AGENT))
        .unwrap()
        .0;
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })
        .unwrap();
    assert!(!plane.attach_session(AttachTarget::ByName("beta".into())));
    let _ = plane.take_events();
    plane
}

#[test]
fn reconnect_does_not_treat_old_own_spawn_as_a_current_parent_admission() {
    let mut plane = followed_own_spawn();
    let attach_id = reconnect_to_beta(&mut plane);
    assert_eq!(close_count(&mut plane), 0);
    ready(&mut plane, attach_id, ResourceId::local(8));
    assert_eq!(close_count(&mut plane), 1);
    assert!(!plane.terminal_is_admitted(&terminal()));
    assert!(!plane.engine().unwrap().is_closed(&ResourceId::local(AGENT)));
}

#[test]
fn replayed_spawn_admission_settles_before_orphan_retirement() {
    for admitted in [false, true] {
        let mut plane = followed_own_spawn();
        // Admission settlement includes keyed spawns, even outside the new
        // home session. This spawn is distinct from the old parent @7.
        let request_id = plane.spawn_terminal(SpawnRequest {
            session_id: Some(1),
            ..SpawnRequest::default()
        });
        let _ = plane.take_outbound();
        assert_eq!(close_count(&mut plane), 0);
        plane.connection_opened();
        let _ = plane.take_outbound();
        plane
            .feed(hello_ok_with(&[ServerFeature::SpawnIdempotency]))
            .unwrap();
        let attach_id = outbound_attach_id(&mut plane);
        attach_to(&mut plane, attach_id, beta_snapshot());
        ready(&mut plane, attach_id, ResourceId::local(8));
        assert_eq!(close_count(&mut plane), 0, "spawn replay remains pending");
        assert!(plane.take_outbound().iter().any(|bytes| matches!(decode(bytes), FrameKind::SpawnResource { request_id: id, .. } if id == request_id)));
        plane
            .feed(FrameKind::ResourceSpawned {
                request_id,
                result: if admitted {
                    SpawnResult::Replayed {
                        id: ResourceId::local(9),
                        instance: None,
                    }
                } else {
                    SpawnResult::Err(phux_protocol::wire::frame::SpawnError::GroupNotFound)
                },
            })
            .unwrap();
        assert_eq!(close_count(&mut plane), 1);
        assert_eq!(plane.terminal_is_admitted(&ResourceId::local(9)), admitted);
        assert_eq!(close_count(&mut plane), 0);
    }
}
