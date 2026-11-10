// -----------------------------------------------------------------------------
// Held verbs beyond `?signal` (ADR-0128, `docs/agents.md#safety`).
// -----------------------------------------------------------------------------

fn admission(
    world: &mut World,
    scopes: &[&str],
    command: &Command,
) -> Result<super::Admission, super::Denial> {
    world.state.set_connection_grant(CLIENT, scoped(scopes));
    super::authorize_command(&world.state, CLIENT, command)
}

fn session_kill(world: &World) -> Command {
    Command::SessionKill {
        operation: phux_protocol::ids::OperationId::new([6; 16]).unwrap(),
        session: world.org.session,
    }
}

/// `?terminate` holds the rows that need `TERMINATE` and nothing else; an
/// un-held grant that also covers the subject runs it at once.
#[test]
fn held_terminate_holds_only_terminating_commands() {
    use super::Admission::{Hold, Run};
    let mut world = world();
    let kill = session_kill(&world);
    assert_eq!(admission(&mut world, &["?terminate@global"], &kill), Ok(Hold));
    assert_eq!(admission(&mut world, &["terminate@global"], &kill), Ok(Run));
    let group = format!("terminate@group:{}", world.alpha_group);
    assert_eq!(
        admission(&mut world, &["?terminate@global", &group], &kill),
        Ok(Run),
        "an un-held clause on the subject wins"
    );
    let signal = Command::KillResource {
        terminal_id: world.alpha.clone(),
        operation_id: None,
    };
    assert_eq!(
        admission(&mut world, &["signal,?terminate@global"], &signal),
        Ok(Run),
        "`?terminate` does not hold SIGNAL"
    );
    let screen = get_screen(world.alpha.clone());
    assert_eq!(
        admission(&mut world, &["observe,?terminate@global"], &screen),
        Ok(Run)
    );
}

/// `?input@CLASS` holds document input to that class and nothing else.
#[test]
fn held_document_input_holds_send_document_input() {
    use super::Admission::{Hold, Run};
    use phux_protocol::wire::frame::ControlClass::{Display, Window};
    let mut world = world();
    let display = document(&mut world, Some(Display));
    let window = document(&mut world, Some(Window));
    let to_display = input_to(&display);
    assert_eq!(admission(&mut world, &["?input@display"], &to_display), Ok(Hold));
    assert_eq!(admission(&mut world, &["input@display"], &to_display), Ok(Run));
    let key = resource_scope(&world, &display);
    assert_eq!(
        admission(&mut world, &["?input@display", &key], &to_display),
        Ok(Run),
        "an un-held grant naming the document wins"
    );
    assert_eq!(
        admission(&mut world, &["?input@display", "input@window"], &input_to(&window)),
        Ok(Run)
    );
    // Terminal input is untouched by a held control class.
    let typed = Command::RouteInput {
        terminal_id: world.alpha.clone(),
        event: InputEvent::Focus(FocusEvent::Gained),
    };
    assert_eq!(
        admission(&mut world, &["input@host", "?input@display"], &typed),
        Ok(Run)
    );
}

/// A decider needs the held command's own verbs, un-held, on its subject:
/// `SIGNAL` alone no longer releases a held session kill, and a decider whose
/// own `TERMINATE` is held is refused (a frame is never held).
#[test]
fn a_decider_needs_the_held_verbs_unheld() {
    let mut world = world();
    let kill = session_kill(&world);
    let id = world.state.open_approval(ClientId(99), &kill).unwrap().id;
    let decide = FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: id.decide_key(),
        value: phux_protocol::wire::frame::APPROVAL_APPROVE.to_vec(),
    };
    let decides = |world: &mut World, scopes: &[&str]| {
        world.state.set_connection_grant(CLIENT, scoped(scopes));
        super::authorize_frame(&world.state, CLIENT, &decide)
    };
    assert!(decides(&mut world, &["terminate@global"]).is_ok());
    assert!(decides(&mut world, &["signal@global"]).is_err());
    let held = decides(&mut world, &["signal,?terminate@global"]).unwrap_err();
    assert_eq!(held.subject, "held");
    world
        .state
        .set_connection_grant(CLIENT, ConnectionGrant::owner());
    assert!(super::authorize_frame(&world.state, CLIENT, &decide).is_ok());
}
