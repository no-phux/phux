// -----------------------------------------------------------------------------
// Watching a window or display needs its class grant (`docs/authority.md`).
// -----------------------------------------------------------------------------

fn attach_resource(target: &WireResourceId) -> Command {
    Command::attach_terminal(target.clone(), None)
}

#[test]
fn watching_a_window_or_display_needs_a_class_grant_never_a_terminal_one() {
    use phux_protocol::wire::frame::ControlClass::{Display, Page, Window};
    let mut world = world();
    let display = document(&mut world, Some(Display));
    let window = document(&mut world, Some(Window));
    let page = document(&mut world, Some(Page));
    let plain = document(&mut world, None);
    let paired = crate::workload::PAIRED_DEVICE_SCOPE;
    let watches = |scopes: &[&str], target: &WireResourceId| {
        let grant = scoped(scopes);
        admits_command(&world, &grant, attach_resource(target))
            && admits_command(&world, &grant, get_screen(target.clone()))
            && enforce(&world.state, CLIENT, &grant, Request::StreamBind(target)).is_ok()
    };

    // Full control of the host watches pages and classless documents
    // through their parent, never a window or display.
    assert!(watches(&[paired], &page));
    assert!(watches(&[paired], &plain));
    assert!(!watches(&[paired], &display));
    assert!(!watches(&[paired], &window));
    let terminal_scope = format!("*@terminal:{}", local_id(&world.alpha));
    assert!(!watches(&[&terminal_scope], &display));

    // `observe@display` watches this host's displays, not its windows.
    assert!(watches(&[paired, "observe@display"], &display));
    assert!(!watches(&[paired, "observe@display"], &window));
    assert!(!watches(&[paired, "observe@display:powerhouse-1"], &display));
    assert!(!watches(&[paired, "input@display"], &display), "the verb is OBSERVE");
    // The class grant does not stand in for the attach's BIND.
    assert!(!watches(&["observe@display"], &display));

    // `observe@resource:KEY` watches exactly the document it names.
    let key = resource_scope(&world, &window).replace("input@", "observe@");
    assert!(watches(&[paired, &key], &window));
    assert!(!watches(&[paired, &key], &display));

    // The owner socket watches everything.
    for target in [&display, &window, &page, &plain] {
        assert!(admits_command(&world, &ConnectionGrant::owner(), attach_resource(target)));
    }
}

#[test]
fn a_refused_watch_names_its_class_only_to_a_caller_who_could_see_it() {
    use phux_protocol::wire::frame::ControlClass::Display;
    let mut world = world();
    let display = document(&mut world, Some(Display));
    let denial = |scopes: &[&str]| {
        let command = attach_resource(&display);
        enforce(&world.state, CLIENT, &scoped(scopes), Request::Command(&command)).unwrap_err()
    };
    assert_eq!(denial(&[crate::workload::PAIRED_DEVICE_SCOPE]).subject, "display");
    assert_eq!(denial(&["inventory@global"]).subject, "terminal");
}

#[test]
fn inventory_still_shows_a_display_the_grant_cannot_watch() {
    use phux_protocol::wire::frame::ControlClass::Display;
    let mut world = world();
    let display = document(&mut world, Some(Display));
    let seen = filtered(&mut world, &[crate::workload::PAIRED_DEVICE_SCOPE], Verb::Inventory);
    let row = seen.resources.iter().find(|row| row.id == display);
    assert_eq!(row.and_then(|row| row.control), Some(Display));
}

#[test]
fn a_hub_admits_a_satellite_watch_by_the_class_the_satellite_reports() {
    use phux_protocol::wire::frame::ControlClass::{Display, Page, Window};
    let mut world = world();
    let mut watches = |scopes: &[&str], class| {
        world.state.set_connection_grant(CLIENT, scoped(scopes));
        super::filter::admits_satellite_watch(&world.state, CLIENT, "powerhouse-1", class)
    };
    let host = "*@host:powerhouse-1";
    assert!(watches(&[host], Some(Page)));
    assert!(watches(&[host], None));
    assert!(!watches(&[host], Some(Window)));
    assert!(!watches(&[host, "observe@display"], Some(Display)), "local only");
    assert!(watches(&[host, "observe@window:powerhouse-1"], Some(Window)));
    assert!(!watches(&[host, "observe@window:powerhouse-1"], Some(Display)));
    assert!(!watches(&[host, "observe@window:other"], Some(Window)));
    world.state.set_connection_grant(CLIENT, ConnectionGrant::owner());
    assert!(super::filter::admits_satellite_watch(
        &world.state,
        CLIENT,
        "powerhouse-1",
        Some(Display)
    ));
}
