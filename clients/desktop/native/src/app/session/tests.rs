use super::{Desktop, WindowRole};
use crate::app::actions::Run;
use gpui::prelude::*;
use gpui::{AppContext, Context, Entity, FocusHandle, Focusable, TestAppContext, Window, div};
use gpui_kit::component::input::{Input, InputState};

mod diagnostics;
mod keys;

#[gpui::test]
fn opening_a_window_does_not_read_its_leased_desktop(cx: &mut TestAppContext) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-update-window-{}", std::process::id()));
    let options = crate::app::AppOptions {
        socket: path.join("isolated.sock"),
        session: String::new(),
        phux: path.join("not-installed-phux"),
        layout_path: path.join("layout.json"),
        fresh: false,
        initial_terminal: None,
        quick: false,
        dev_telemetry: false,
        startup_error: Some("Isolated window fixture".into()),
        cli_install: None,
    };
    desktop.update(cx, |desktop, cx| {
        desktop.launch(options, cx).unwrap();
        assert_eq!(desktop.windows.len(), 1);
        assert_eq!(
            desktop.windows[0].desk.read(cx).updates.entity_id(),
            desktop.updates.entity_id()
        );
        assert!(desktop.updates.read(cx).development);
    });
}

#[test]
fn next_window_cycles_same_server_peers_and_never_picks_the_source() {
    use super::next_peer;
    let peers = [
        (1, None, true),
        (2, Some("mini"), true),
        (3, None, true),
        (4, None, true),
    ];
    assert_eq!(next_peer(&peers, 1, None), Some(2));
    assert_eq!(next_peer(&peers, 3, None), Some(3));
    assert_eq!(next_peer(&peers, 4, None), Some(0));
    // Another authority's window is never a target.
    assert_eq!(next_peer(&peers, 1, Some("mini")), Some(1));
    assert_eq!(next_peer(&peers, 2, Some("mini")), None);
    // An unlisted source (the quick window) starts at the front.
    assert_eq!(next_peer(&peers, 9, None), Some(0));
    assert_eq!(next_peer::<u8>(&[], 1, None), None);
}

#[test]
fn every_menu_bar_item_carries_its_catalog_title_once() {
    use super::{MENU_BAR, MENU_SEPARATOR};
    use crate::app::actions::title;
    for (menu, ids) in MENU_BAR {
        let mut titles: Vec<&str> = ids
            .iter()
            .filter(|id| **id != MENU_SEPARATOR)
            .map(|id| title(id))
            .collect();
        let count = titles.len();
        titles.sort_unstable();
        titles.dedup();
        assert_eq!(titles.len(), count, "{menu} names each command once");
    }
    assert_eq!(title("settings"), "Settings…");
    assert_eq!(title("find"), "Find in Terminal");
}

#[test]
fn next_window_skips_peers_that_cannot_take_the_terminal_yet() {
    use super::next_peer;
    // Window 2 is still connecting (or failed, or already revealing another
    // moved terminal); releasing the source view into it would strand the
    // terminal with no view in either window.
    let peers = [(1, None, false), (2, None, false), (3, None, true)];
    assert_eq!(next_peer(&peers, 1, None), Some(2));
    assert_eq!(next_peer(&peers, 3, None), None);
    let none_ready = [(1, None, false), (2, None, false)];
    assert_eq!(next_peer(&none_ready, 1, None), None);
}

#[gpui::test]
fn project_open_uses_a_durable_window_even_when_a_fresh_view_exists(cx: &mut TestAppContext) {
    // The role check does not need a server. A missing socket still makes
    // `Runtime::connect` spawn its driver, and that driver's ActivityListener
    // wakes GPUI's TestScheduler off the test thread. Hold an embedded client
    // for this fixture only.
    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-project-window-{}", std::process::id()));
    let options = crate::app::AppOptions {
        socket: path.join("isolated.sock"),
        session: String::new(),
        phux: path.join("not-installed-phux"),
        layout_path: path.join("layout.json"),
        fresh: false,
        initial_terminal: None,
        quick: false,
        dev_telemetry: false,
        startup_error: Some("Isolated window fixture".into()),
        cli_install: None,
    };
    desktop.update(cx, |desktop, cx| {
        desktop.launch(options, cx).unwrap();
        desktop.open_session("Cedar".into(), None, cx);
        assert_eq!(desktop.windows[1].role, WindowRole::Fresh);
        desktop
            .open_project("Cedar".into(), phux_protocol::ResourceId::local(1), cx)
            .unwrap();
        assert_eq!(desktop.windows.len(), 3);
        assert_eq!(desktop.windows[2].role, WindowRole::Main);
        assert_eq!(desktop.windows[2].session, "Cedar");
    });
}

#[gpui::test]
fn performance_monitor_starts_stops_and_reopens_live_without_a_launch_flag(
    cx: &mut TestAppContext,
) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-performance-window-{}", std::process::id()));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = crate::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&crate::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let main = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                crate::app::AppOptions {
                    socket: path.join("isolated.sock"),
                    session: String::new(),
                    phux: path.join("not-installed-phux"),
                    layout_path: path.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: None,
                    cli_install: None,
                },
                cx,
            )
            .unwrap()
    });
    cx.run_until_parked();
    let run = |id: &str, cx: &mut TestAppContext| {
        main.update(cx, |_, window, cx| {
            window.activate_window();
            window.dispatch_action(Box::new(Run::new(id)), cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    run("performance-monitor-stop", cx);
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    run("performance-monitor", cx);
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    let first = desktop.read_with(cx, |desktop, _| desktop.performance.unwrap());
    run("performance-monitor", cx);
    assert_eq!(
        desktop.read_with(cx, |desktop, _| desktop.performance.unwrap().window_id()),
        first.window_id(),
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    run("performance-monitor-stop", cx);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    assert!(
        cx.update_window(first, |_, _, _| ()).is_err(),
        "stop closes the sampler's owning window"
    );
    run("performance-monitor", cx);
    let reopened = desktop.read_with(cx, |desktop, _| desktop.performance.unwrap());
    assert_ne!(reopened.window_id(), first.window_id());
    cx.update_window(reopened, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    run("performance-monitor", cx);
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    run("performance-monitor-stop", cx);
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
}

#[gpui::test]
fn main_menu_drops_from_the_titlebar_with_or_without_a_sidebar(cx: &mut TestAppContext) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-main-hub-{}", std::process::id()));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = crate::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&crate::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let handle = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                crate::app::AppOptions {
                    socket: path.join("isolated.sock"),
                    session: String::new(),
                    phux: path.join("not-installed-phux"),
                    layout_path: path.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    startup_error: Some("Isolated hub fixture".into()),
                    dev_telemetry: false,
                    cli_install: None,
                },
                cx,
            )
            .unwrap()
    });
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    let cx = &mut gpui::VisualTestContext::from_window(*handle, cx);
    for visible in [true, false] {
        desk.update(cx, |desk, cx| {
            desk.prefs.sidebar_visible = visible;
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds("main-menu-rail")
            .expect("persistent main menu trigger");
        assert!(bounds.origin.x >= gpui::px(78.));
        assert!(bounds.right() <= gpui::px(240.));
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            crate::app::Modal::None,
            "the main menu is a dropdown, not a modal"
        );
        let menu = desk
            .read_with(cx, |desk, _| desk.menu_window())
            .expect("the glass main menu opens");
        let menu_cx = &mut gpui::VisualTestContext::from_window(menu, cx);
        let settings = menu_cx
            .debug_bounds("glass-menu-settings")
            .expect("settings in the main menu");
        menu_cx.simulate_click(settings.center(), gpui::Modifiers::none());
        menu_cx.run_until_parked();
        assert_eq!(
            desk.read_with(menu_cx, |desk, _| desk.modal.clone()),
            crate::app::Modal::None
        );
        assert!(desk.read_with(menu_cx, |desk, cx| desk.desktop.read(cx).settings.is_some()));
        assert!(
            desk.read_with(menu_cx, |desk, _| desk.menu_window())
                .is_none(),
            "choosing an item closes the menu"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            crate::app::Modal::None
        );
        assert_eq!(
            desktop.read_with(cx, |desktop, _| desktop.windows.len()),
            1,
            "navigation opens no extra native windows"
        );
    }
}

/// ADR-0128: a server-held approval is an Inbox entry with Approve and Deny.
/// Approving sends one decision; the server's confirmation takes the entry
/// down and reports the decision, and nothing is sent again.
#[gpui::test]
fn the_inbox_shows_a_pending_approval_and_clears_it_after_a_decision(cx: &mut TestAppContext) {
    use phux_protocol::caps::{
        BootstrapLimits, BootstrapProfile, Layer, LayerSet, ServerCapabilities, ServerFeature,
        ServerFeatureSet,
    };
    use phux_protocol::ids::ApprovalId;
    use phux_protocol::wire::frame::{FrameKind, Scope};

    use crate::app::notifications::Kind;

    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-approval-inbox-{}", std::process::id()));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = crate::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&crate::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let handle = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                crate::app::AppOptions {
                    socket: path.join("isolated.sock"),
                    session: "work".into(),
                    phux: path.join("not-installed-phux"),
                    layout_path: path.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    startup_error: None,
                    dev_telemetry: false,
                    cli_install: None,
                },
                cx,
            )
            .unwrap()
    });
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    let cx = &mut gpui::VisualTestContext::from_window(*handle, cx);
    let client = desk.read_with(cx, |desk, _| {
        std::sync::Arc::clone(
            &desk
                .link
                .as_ref()
                .expect("an embedded link")
                .connection
                .client,
        )
    });
    let sent = |client: &phux_client_runtime::Client| -> Vec<FrameKind> {
        client
            .take_outbound()
            .iter()
            .map(|bytes| FrameKind::decode(bytes).unwrap().0)
            .collect()
    };
    let feed = |client: &phux_client_runtime::Client, frame: FrameKind| {
        client.feed(frame).unwrap();
        client.wake();
    };

    client.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    let _ = sent(&client);
    feed(
        &client,
        FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_layers(LayerSet::with(&[Layer::L3]))
                .with_features(ServerFeatureSet::with(&[ServerFeature::Approvals])),
            server_id: vec![0xAB; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        },
    );
    let listing = sent(&client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::ListMetadata {
                request_id,
                scope: Scope::Global,
            } => Some(request_id),
            _ => None,
        })
        .expect("the desktop watches approvals");
    let id = ApprovalId::new([0x5a; 16]).unwrap();
    feed(
        &client,
        FrameKind::MetadataKeys {
            request_id: listing,
            keys: vec![id.record_key()],
        },
    );
    let read = sent(&client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::GetMetadata {
                request_id, key, ..
            } if key == id.record_key() => Some(request_id),
            _ => None,
        })
        .expect("the listed record is read");
    let record = format!(
        r#"{{"schema_version":1,"id":"{id}","requester":{{"client":3,"client_name":"agent"}},"method":"KILL_RESOURCE","subjects":["terminal:4"],"requested_at_ms":1,"expires_at_ms":120001}}"#
    );
    feed(
        &client,
        FrameKind::MetadataValue {
            request_id: read,
            value: Some(record.into_bytes()),
        },
    );
    cx.run_until_parked();

    let pending = desk.read_with(cx, |desk, _| {
        desk.feed
            .items()
            .filter(|entry| entry.kind == Kind::Approval)
            .map(|entry| (entry.title.clone(), entry.approval))
            .collect::<Vec<_>>()
    });
    assert_eq!(pending, vec![("Approve kill?".to_owned(), Some(id))]);

    desk.update_in(cx, |desk, window, cx| {
        desk.execute("notifications", window, cx)
    });
    cx.run_until_parked();
    let approve = cx
        .debug_bounds("approval-approve")
        .expect("the Inbox entry offers Approve");
    cx.simulate_click(approve.center(), gpui::Modifiers::none());
    cx.run_until_parked();

    let decision = sent(&client);
    let confirm = match decision.as_slice() {
        [
            FrameKind::SetMetadata {
                scope: Scope::Global,
                key,
                value,
                ..
            },
            FrameKind::GetMetadata { request_id, .. },
        ] if *key == id.decide_key() && value.as_slice() == b"approve" => *request_id,
        other => panic!("one decision and its confirming read: {other:?}"),
    };
    feed(
        &client,
        FrameKind::MetadataValue {
            request_id: confirm,
            value: None,
        },
    );
    cx.run_until_parked();

    desk.read_with(cx, |desk, _| {
        assert!(
            !desk.feed.items().any(|entry| entry.kind == Kind::Approval),
            "a decided approval leaves the Inbox"
        );
        assert!(
            desk.feed
                .items()
                .any(|entry| entry.title == "Approved kill"),
            "the decision is reported"
        );
    });
    assert!(sent(&client).is_empty(), "nothing is sent again");
}
