// -----------------------------------------------------------------------------
// Document input: control is never inherited (`docs/resource-kinds.md#authority`).
// -----------------------------------------------------------------------------

/// A pixel surface under `alpha` whose producer declared `control`.
fn document(world: &mut World, control: Option<phux_protocol::wire::frame::ControlClass>) -> WireResourceId {
    let core = world
        .state
        .create_document(
            world.alpha_core,
            phux_core::LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
                target: "smoke".to_owned(),
            }),
            &phux_core::Actor::owner(),
        )
        .unwrap();
    world.state.set_document_control(core, control);
    world.state.intern_resource_wire(core)
}

fn input_to(document: &WireResourceId) -> Command {
    Command::SendDocumentInput {
        terminal_id: document.clone(),
        intent: phux_protocol::wire::frame::DocumentInput::Click { x: 1, y: 1 },
        control: None,
    }
}

fn resource_scope(world: &World, document: &WireResourceId) -> String {
    let core = world.state.resource_from_wire(document).unwrap();
    let key = crate::org_wire::resource_key(world.state.registry().resource_key(core).unwrap());
    format!("input@resource:{}", key.unwrap())
}

#[test]
fn document_input_needs_a_control_grant_never_a_terminal_one() {
    use phux_protocol::wire::frame::ControlClass::{Display, Page, Window};
    let mut world = world();
    let display = document(&mut world, Some(Display));
    let window = document(&mut world, Some(Window));
    let page = document(&mut world, Some(Page));
    let plain = document(&mut world, None);
    let satellite_window = WireResourceId::satellite("powerhouse-1", 5);
    let admits = |scopes: &[&str], target: &WireResourceId| {
        admits_command(&world, &scoped(scopes), input_to(target))
    };

    // Full control of every Terminal, the paired-device ceiling, and every
    // broader selector: none of it reaches a display, nor any document.
    let terminal_scope = format!("*@terminal:{}", local_id(&world.alpha));
    let group_scope = format!("*@group:{}", world.alpha_group);
    let terminal_grants: [&[&str]; 5] = [
        &[crate::workload::PAIRED_DEVICE_SCOPE],
        &["*@host"],
        &[&terminal_scope],
        &[&group_scope],
        &["*@host:powerhouse-1"],
    ];
    for target in [&display, &window, &page, &plain, &satellite_window] {
        for grant in terminal_grants {
            assert!(!admits(grant, target), "{grant:?} reached {target}");
        }
    }

    // `input@display` reaches this host's displays, not its windows, and
    // no satellite's.
    assert!(admits(&["input@display"], &display));
    assert!(!admits(&["input@display"], &window));
    assert!(!admits(&["input@display"], &satellite_window));
    assert!(!admits(&["observe@display"], &display), "the verb is INPUT");
    assert!(admits(&["input@page", "input@window"], &page));

    // A satellite form reaches only that satellite's documents.
    assert!(admits(&["input@window:powerhouse-1"], &satellite_window));
    assert!(!admits(&["input@window:other"], &satellite_window));
    assert!(!admits(&["input@window:powerhouse-1"], &window));

    // `resource:KEY` reaches exactly the document it names.
    let display_key = resource_scope(&world, &display);
    assert!(admits(&[&display_key], &display));
    assert!(!admits(&[&display_key], &window));

    // A document that controls nothing refuses every grant.
    let plain_key = resource_scope(&world, &plain);
    for grant in [&["*@global"][..], &["input@page", "input@window", "input@display"], &[&plain_key]] {
        assert!(!admits(grant, &plain), "{grant:?} reached a document with no control class");
    }

    // The owner socket reaches every one of them.
    for target in [&display, &window, &page, &plain, &satellite_window] {
        assert!(admits_command(&world, &ConnectionGrant::owner(), input_to(target)));
    }
}

#[test]
fn the_clipboard_needs_the_same_control_grant_as_any_input() {
    use phux_protocol::wire::frame::ControlClass::Display;
    use phux_protocol::wire::frame::DocumentInput;
    let mut world = world();
    let display = document(&mut world, Some(Display));
    let clipboard = [
        DocumentInput::CopyClipboard { reply: 0 },
        DocumentInput::Paste { text: "x".into(), chord: true },
        DocumentInput::ClipboardText { reply: 1, text: Ok("x".into()) },
    ];
    for intent in clipboard {
        let command = || Command::SendDocumentInput {
            terminal_id: display.clone(),
            intent: intent.clone(),
            control: None,
        };
        let terminal_scope = format!("*@terminal:{}", local_id(&world.alpha));
        for grant in [&[][..], &["observe@display"], &["*@host"], &[&terminal_scope]] {
            assert!(!admits_command(&world, &scoped(grant), command()), "{grant:?} reached {intent:?}");
        }
        assert!(!admits_command(&world, &scoped(&["input@window"]), command()), "a display is not a window");
        assert!(admits_command(&world, &scoped(&["input@display"]), command()));
    }
}

#[test]
fn a_hub_stamps_exactly_the_satellite_classes_the_grant_names() {
    use phux_protocol::wire::frame::{ControlClass, ControlClasses};
    let mut world = world();
    let classes = |world: &mut World, scopes: &[&str]| {
        world.state.set_connection_grant(CLIENT, scoped(scopes));
        super::filter::satellite_input_classes(&world.state, CLIENT, "powerhouse-1")
    };
    let only = |class| Some(ControlClasses::EMPTY.with(class));
    assert_eq!(classes(&mut world, &["input@window:powerhouse-1"]), only(ControlClass::Window));
    assert_eq!(
        classes(&mut world, &["input@display:powerhouse-1", "input@display"]),
        only(ControlClass::Display)
    );
    assert_eq!(
        classes(&mut world, &[crate::workload::PAIRED_DEVICE_SCOPE]),
        Some(ControlClasses::EMPTY)
    );
    world.state.set_connection_grant(CLIENT, ConnectionGrant::owner());
    assert_eq!(super::filter::satellite_input_classes(&world.state, CLIENT, "powerhouse-1"), None);
}
