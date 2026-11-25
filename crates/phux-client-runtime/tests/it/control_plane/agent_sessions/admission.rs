//! Agent subscriptions follow exact parent admission and retirement.

use super::*;

#[test]
fn an_agent_session_is_subscribed_once_its_pane_is_streamed() {
    let (mut plane, attach_id) = subscribing(true);
    // The agent runs in a pane this attach does not stream.
    attach_to(&mut plane, attach_id, with_agent(ResourceId::local(8)));
    assert!(attach_resources(&mut plane).is_empty());

    let pane_request = plane.attach_terminal(&ResourceId::local(8));
    assert_eq!(
        attach_resources(&mut plane),
        vec![(pane_request, ResourceId::local(8))]
    );
    plane.reply(pane_request, CommandResult::Ok);
    let subscribed = attach_resources(&mut plane);
    assert_eq!(subscribed.len(), 1);
    assert_eq!(subscribed[0].1, ResourceId::local(AGENT));
}

#[test]
fn without_the_option_the_runtime_subscribes_no_agent_session() {
    let (mut plane, attach_id) = subscribing(false);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    assert!(attach_resources(&mut plane).is_empty());
}

#[test]
fn reconnect_after_switch_retires_home_subscription_once_and_can_follow_it_again() {
    let mut plane = followed_home();
    let attach_id = reconnect_to_beta(&mut plane);
    assert_eq!(close_count(&mut plane), 0, "wait for ATTACH_READY");
    ready(&mut plane, attach_id, &ResourceId::local(8));
    assert_eq!(close_count(&mut plane), 1);
    assert!(!plane.engine().unwrap().is_closed(&ResourceId::local(AGENT)));

    let request_id = plane.refresh_topology().unwrap();
    // ATTACHED fences its earlier inventory; answer that and then the fresh read.
    plane.reply(request_id, state(with_agent(terminal())));
    assert_eq!(close_count(&mut plane), 0);
    let request_id = plane.attach_terminal(&terminal());
    let _ = plane.take_outbound();
    plane.reply(request_id, CommandResult::Ok);
    let subscriptions = attach_resources(&mut plane);
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].1, ResourceId::local(AGENT));
    assert_eq!(close_count(&mut plane), 0);
}

#[test]
fn reconnect_waits_for_preserving_parent_admission_and_retires_only_if_refused() {
    for refused in [false, true] {
        let (mut plane, attach_id) = subscribing(true);
        attach_to(&mut plane, attach_id, beta_snapshot());
        ready(&mut plane, attach_id, &ResourceId::local(8));
        // Explicit geometry-preserving admission outside the attached session
        // survives reconnect, unlike an ordinary old home/spawn pump.
        let request_id = plane.attach_terminal_preserving_geometry(&terminal());
        plane.reply(request_id, CommandResult::Ok);
        confirm_agent_subscription(&mut plane);
        // Set the reconnect target to the current home session.
        assert!(!plane.attach_session(AttachTarget::ByName("beta".into())));
        let _ = plane.take_outbound();
        let _ = plane.take_events();
        let attach_id = reconnect_to_beta(&mut plane);
        ready(&mut plane, attach_id, &ResourceId::local(8));
        assert_eq!(close_count(&mut plane), 0, "replayed attach still pending");
        let requests = attach_resources(&mut plane);
        let request_id = requests.iter().find(|(_, id)| *id == terminal()).unwrap().0;
        // An inventory may optimistically subscribe the agent while its
        // parent's replay is pending; a refusal must still retire it.
        let inventory_request = plane.refresh_topology().unwrap();
        plane.reply(inventory_request, state(beta_snapshot()));
        let subscriptions = attach_resources(&mut plane);
        assert_eq!(subscriptions.len(), 1);
        assert_eq!(subscriptions[0].1, ResourceId::local(AGENT));
        plane.reply(subscriptions[0].0, CommandResult::Ok);
        assert_eq!(close_count(&mut plane), 0);
        plane.reply(request_id, refused_or_ok(refused, "denied"));
        assert_eq!(close_count(&mut plane), usize::from(refused));
        assert!(attach_resources(&mut plane).is_empty());
    }
}

#[test]
fn refused_parent_detach_preserves_agent_subscription_and_success_releases_once() {
    for refused in [false, true] {
        let (mut plane, attach_id) = subscribing(true);
        attach_to(&mut plane, attach_id, with_agent(ResourceId::local(8)));
        ready(&mut plane, attach_id, &terminal());
        let request_id = plane.attach_terminal(&ResourceId::local(8));
        plane.reply(request_id, CommandResult::Ok);
        confirm_agent_subscription(&mut plane);
        assert_eq!(close_count(&mut plane), 0);

        let detach_request = plane.detach_terminal(&ResourceId::local(8));
        let inventory_request = plane.refresh_topology().unwrap();
        let _ = plane.take_outbound();
        plane.reply(inventory_request, state(with_agent(ResourceId::local(8))));
        assert_eq!(close_count(&mut plane), 0, "detach admission is unsettled");
        plane.reply(
            detach_request,
            refused_or_ok(refused, "parent detach refused"),
        );
        assert_eq!(plane.terminal_is_admitted(&ResourceId::local(8)), refused);
        assert_eq!(close_count(&mut plane), usize::from(!refused));
        let releases = plane
            .take_outbound()
            .iter()
            .filter(|bytes| {
                matches!(decode(bytes), FrameKind::Command {
                command: Command::DetachResource { terminal_id }, ..
            } if terminal_id == ResourceId::local(AGENT))
            })
            .count();
        assert_eq!(releases, usize::from(!refused));
        assert_eq!(close_count(&mut plane), 0);

        // Refusing a detach for a never-admitted resource must not grant a
        // parent admission as a side effect of the restoration path.
        let request_id = plane.detach_terminal(&ResourceId::local(99));
        plane
            .feed(command_err(
                request_id,
                ErrorCode::PermissionDenied,
                "unknown parent",
            ))
            .unwrap();
        assert!(!plane.terminal_is_admitted(&ResourceId::local(99)));
        assert_eq!(close_count(&mut plane), 0);
    }
}

#[test]
fn parent_close_during_detach_cannot_be_undone_by_a_late_refusal() {
    let parent = ResourceId::local(8);
    let (mut plane, attach_id) = subscribing(true);
    attach_to(&mut plane, attach_id, with_agent(parent.clone()));
    ready(&mut plane, attach_id, &terminal());
    let request_id = plane.attach_terminal(&parent);
    plane.reply(request_id, CommandResult::Ok);
    confirm_agent_subscription(&mut plane);
    assert_eq!(close_count(&mut plane), 0);

    let detach_request = plane.detach_terminal(&parent);
    let _ = plane.take_outbound();
    plane
        .feed(phux_wire_fixtures::closed(&parent, Some(0)))
        .unwrap();
    assert!(plane.engine().unwrap().is_closed(&parent));
    plane
        .feed(command_err(
            detach_request,
            ErrorCode::PermissionDenied,
            "detach refused after parent closed",
        ))
        .unwrap();
    assert!(!plane.terminal_is_admitted(&parent));
    assert_eq!(close_count(&mut plane), 1);
    assert!(attach_resources(&mut plane).is_empty());

    // An unversioned inventory can still contain the old AgentSession.
    // Repeated reads must not attach it under the closed parent.
    for _ in 0..3 {
        let request_id = plane.refresh_topology().unwrap();
        plane.reply(request_id, state(with_agent(parent.clone())));
        assert!(attach_resources(&mut plane).is_empty());
        assert_eq!(close_count(&mut plane), 0);
    }
}
