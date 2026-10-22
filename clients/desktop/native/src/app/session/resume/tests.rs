use super::*;
use crate::workspace::persist::{SavedNode, SavedTab};

fn saved(role: WindowRole, session: &str, remote: Option<&str>) -> SavedWindow {
    SavedWindow::from_request(OpenRequest {
        role,
        session: session.into(),
        home_key: Some(SessionKey::new([7; 16]).unwrap()),
        authority: Some(AuthorityId::new([8; 16]).unwrap()),
        remote: remote.map(str::to_owned),
        layout_path: PathBuf::from("/isolated/layout.json"),
        stable_layout: true,
        resume_layout: Some(persist::SavedLayout {
            version: 2,
            server_id: "incarnation".into(),
            active_tab: Some("chat".into()),
            tabs: vec![SavedTab {
                id: "chat".into(),
                title: Some("Current chat".into()),
                root: SavedNode::Leaf {
                    id: Some("focused-pane".into()),
                    terminal_id: "17".into(),
                },
                focused_terminal: "17".into(),
                focused_id: Some("focused-pane".into()),
            }],
            display: settings::default_display(),
        }),
        ..OpenRequest::default()
    })
}

#[test]
fn restart_roundtrip_preserves_remote_identity_layout_and_foreground_window() {
    let windows = vec![
        saved(WindowRole::Fresh, "Chat", Some("workstation")),
        saved(WindowRole::Main, "Intake", None),
        saved(WindowRole::Quick, "Scratch", None),
    ];
    let expected = windows[0].layout.clone();
    let value = serde_json::to_value(Restart {
        version: 1,
        windows,
        active: Some(0),
    })
    .unwrap();
    let requests = restart_requests(value).unwrap();
    let chat = requests.last().unwrap();
    assert_eq!(chat.session, "Chat");
    assert_eq!(chat.remote.as_deref(), Some("workstation"));
    assert_eq!(chat.home_key, Some(SessionKey::new([7; 16]).unwrap()));
    assert_eq!(chat.authority, Some(AuthorityId::new([8; 16]).unwrap()));
    assert_eq!(chat.role, WindowRole::Fresh);
    assert!(
        !chat.fresh,
        "resuming must never spawn a replacement terminal"
    );
    assert_eq!(
        chat.resume_layout.as_ref().map(persist::layout_value),
        expected
    );
    assert_eq!(requests[1].role, WindowRole::Quick);
}

#[test]
fn malformed_restart_is_rejected_before_any_window_opens() {
    let value = || {
        serde_json::to_value(Restart {
            version: 1,
            windows: vec![saved(WindowRole::Main, "Chat", None)],
            active: Some(0),
        })
        .unwrap()
    };
    let mut future = value();
    future["version"] = 99.into();
    assert!(restart_requests(future).is_err());
    let mut invalid_focus = value();
    invalid_focus["active"] = 8.into();
    assert!(restart_requests(invalid_focus).is_err());
    let mut broken_layout = value();
    broken_layout["windows"][0]["layout"]["version"] = 99.into();
    assert!(restart_requests(broken_layout).is_err());
    let mut missing_session = value();
    missing_session["windows"][0]["session"] = "".into();
    assert!(restart_requests(missing_session).is_err());
    let duplicate_quick = serde_json::to_value(Restart {
        version: 1,
        windows: vec![
            saved(WindowRole::Quick, "A", None),
            saved(WindowRole::Quick, "B", None),
        ],
        active: None,
    })
    .unwrap();
    assert!(restart_requests(duplicate_quick).is_err());
}

#[gpui::test]
fn untargeted_launch_consumes_restart_once_and_explicit_launch_leaves_it_alone(
    cx: &mut gpui::TestAppContext,
) {
    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let root = std::env::temp_dir().join(format!("phux-resume-{}", std::process::id()));
    let options = || AppOptions {
        socket: root.join("isolated.sock"),
        session: "Intake".into(),
        phux: root.join("not-installed"),
        layout_path: PathBuf::new(),
        fresh: false,
        initial_terminal: None,
        quick: false,
        dev_telemetry: false,
        startup_error: None,
    };
    let store = desktop.update(cx, |desktop, cx| {
        desktop.launch(options(), cx).unwrap();
        desktop.restart_store()
    });
    store
        .write(
            &serde_json::to_value(Restart {
                version: 1,
                windows: vec![saved(
                    WindowRole::Fresh,
                    "Current chat",
                    Some("workstation"),
                )],
                active: Some(0),
            })
            .unwrap(),
        )
        .unwrap();
    let explicit = cx.update(Desktop::install);
    explicit.update(cx, |desktop, cx| {
        desktop.launch(options(), cx).unwrap();
    });
    assert!(store.path.exists());
    let restored = cx.update(Desktop::install);
    restored.update(cx, |desktop, cx| {
        desktop.launch_with_restore(options(), true, cx).unwrap();
        assert_eq!(desktop.windows.len(), 1);
        let desk = desktop.windows[0].desk.read(cx);
        assert_eq!(desk.session, "Current chat");
        assert_eq!(desk.remote.as_deref(), Some("workstation"));
        assert_eq!(desk.role, WindowRole::Fresh);
    });
    assert!(
        !store.path.exists(),
        "a later ordinary launch must not replay an old update"
    );
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn update_click_waits_for_the_desk_lease_before_saving(cx: &mut gpui::TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let root = std::env::temp_dir().join(format!("phux-update-click-{}", std::process::id()));
    let (desk, store) = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                AppOptions {
                    socket: root.join("isolated.sock"),
                    session: "Chat".into(),
                    phux: root.join("not-installed"),
                    layout_path: root.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: None,
                },
                cx,
            )
            .unwrap();
        (desktop.windows[0].desk.clone(), desktop.restart_store())
    });
    desk.update(cx, |desk, cx| {
        desk.updates.update(cx, |updater, cx| {
            updater.development = false;
            updater.prefs.manage_runtime = false;
            // No app bundle: even shutdown of this fixture cannot install anything.
            updater.bundle = None;
            updater.staged = Some(super::super::super::updates::Staged {
                app: root.join("not-an-app"),
                release: crate::updates::Release::default(),
            });
            updater.act(cx);
            assert!(updater.busy);
        });
    });
    cx.run_until_parked();
    let persist::StoreValue::Present(value) = store.read() else {
        panic!("the actual update click must save a restart snapshot");
    };
    assert_eq!(restart_requests(value).unwrap()[0].session, "Chat");
    std::fs::remove_file(store.path).unwrap();
}

#[gpui::test]
fn closing_the_last_window_keeps_its_location_for_the_staged_update(cx: &mut gpui::TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let root = std::env::temp_dir().join(format!("phux-update-close-{}", std::process::id()));
    let (handle, store) = desktop.update(cx, |desktop, cx| {
        let handle = desktop
            .launch(
                AppOptions {
                    socket: root.join("isolated.sock"),
                    session: "Chat at close".into(),
                    phux: root.join("not-installed"),
                    layout_path: root.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: None,
                },
                cx,
            )
            .unwrap();
        desktop.updates.update(cx, |updater, _| {
            updater.development = false;
            updater.bundle = None;
            updater.staged = Some(super::super::super::updates::Staged {
                app: root.join("not-an-app"),
                release: crate::updates::Release::default(),
            });
        });
        (handle, desktop.restart_store())
    });
    cx.update_window(*handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| cx.shutdown());
    let persist::StoreValue::Present(value) = store.read() else {
        panic!("the updater must snapshot before the final Desk is released");
    };
    let requests = restart_requests(value).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].session, "Chat at close");
    std::fs::remove_file(store.path).unwrap();
}
