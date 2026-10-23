//! Every modal's placement, dismissal and text ownership.

use super::*;

/// The element that carries `modal`'s card.
fn surface(modal: &Modal) -> &'static str {
    match modal {
        Modal::NativeLaunch => "native-launch-panel",
        Modal::AgentFleet => "agent-fleet-panel",
        Modal::Commands
        | Modal::Goto
        | Modal::WindowSession
        | Modal::Windows
        | Modal::MoveResources => "palette-panel",
        _ => "modal-sheet",
    }
}

#[gpui::test]
fn every_modal_shares_one_placement_and_dismissal(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let modals = [
        Modal::Commands,
        Modal::Rename { tab_id: "t".into() },
        Modal::Session {
            create: true,
            current: String::new(),
            target: None,
        },
        Modal::Session {
            create: false,
            current: "work".into(),
            target: None,
        },
        Modal::Project {
            host: None,
            session: phux_protocol::ids::SessionKey::new([1; 16]).unwrap(),
            name: "work".into(),
            current: Some("phux".into()),
        },
        Modal::AddHost,
        Modal::RemoveHost {
            name: "mini".into(),
        },
        Modal::KillSession {
            name: "work".into(),
            session: 1,
            target: None,
        },
        Modal::Notifications,
        Modal::AgentFleet,
        Modal::NativeLaunch,
        Modal::Path,
        Modal::Goto,
        Modal::WindowSession,
        Modal::Windows,
        Modal::MoveResources,
        Modal::Terminate {
            terminal: phux_protocol::ResourceId::local(1),
            title: "shell".into(),
        },
        Modal::Paste {
            placement_id: "place-1".into(),
            terminal_id: phux_protocol::ResourceId::local(1),
            title: "shell".into(),
            paste: crate::input::HeldPaste {
                text: "one\ntwo".into(),
            },
        },
    ];
    let viewport = cx.update(|window, _| window.viewport_size());
    for modal in modals {
        let modal = &modal;
        let selector = surface(modal);
        let open = |cx: &mut VisualTestContext| {
            open(&desk, modal, cx);
        };
        // Every dimmed modal is centered in both axes; the inbox is anchored.
        open(cx);
        let card = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{modal:?} renders its card"));
        if *modal == Modal::Notifications {
            // The inbox drops from the bell instead, right edges aligned.
            let bell = cx.debug_bounds("notifications").expect("the bell");
            assert_eq!(card.top(), bell.bottom() + gpui::px(6.));
            assert_eq!(card.right(), bell.right());
        } else {
            let center = card.center();
            assert!(
                (center.x - viewport.width / 2.).abs() <= gpui::px(0.5)
                    && (center.y - viewport.height / 2.).abs() <= gpui::px(0.5),
                "{modal:?} is centered (within pixel rounding): {center:?}"
            );
        }
        // A click inside the card never dismisses it.
        cx.simulate_click(
            card.origin + gpui::point(gpui::px(4.), gpui::px(4.)),
            gpui::Modifiers::none(),
        );
        render(cx);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            *modal,
            "a click inside {modal:?} keeps it open"
        );
        // Escape dismisses and hands the keyboard back.
        keys(cx, "escape");
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::None,
            "Escape closes {modal:?}"
        );
        assert_desk_focus(&desk, cx);
        // A click outside dismisses the same way.
        open(cx);
        cx.simulate_click(
            gpui::point(gpui::px(8.), gpui::px(8.)),
            gpui::Modifiers::none(),
        );
        render(cx);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::None,
            "an outside click closes {modal:?}"
        );
        assert_desk_focus(&desk, cx);
    }
}

#[gpui::test]
fn text_dialogs_own_typing_until_dismissed(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    for (modal, slot) in [
        (Modal::Rename { tab_id: "t".into() }, InputSlot::Rename),
        (
            Modal::Session {
                create: true,
                current: String::new(),
                target: None,
            },
            InputSlot::SessionName,
        ),
        (Modal::AddHost, InputSlot::HostTarget),
    ] {
        open(&desk, &modal, cx);
        type_text(cx, "cedar");
        desk.read_with(cx, |desk, cx| {
            let input = desk.input_entity(slot).expect("dialog field");
            assert_eq!(input.read(cx).value(), "cedar", "{modal:?} owns typing");
        });
        keys(cx, "escape");
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::None
        );
        assert_desk_focus(&desk, cx);
    }
}

#[gpui::test]
fn path_loading_and_failure_read_in_the_results_not_the_footer(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    open(&desk, &Modal::Path, cx);
    // Debug selectors are `'static`; the notice and status carry their text.
    let selector = |slot: &str, text: &str| -> &'static str {
        Box::leak(format!("{slot}: {text}").into_boxed_str())
    };
    for (picker, text) in [
        (
            crate::connection::path::start_query(path_picker(), 7, "/repo", ""),
            "Searching host paths…",
        ),
        (
            crate::connection::path::PickerState {
                failure: Some("permission denied".into()),
                ..path_picker()
            },
            "permission denied",
        ),
        (
            crate::connection::path::PickerState {
                message: "Host index is warming; search again shortly.".into(),
                ..path_picker()
            },
            "Host index is warming; search again shortly.",
        ),
        (path_picker(), "No paths."),
    ] {
        desk.update(cx, |desk, cx| {
            desk.picker = Some(picker);
            cx.notify();
        });
        render(cx);
        let notice = cx
            .debug_bounds("path-results-notice")
            .expect("the state fills the results");
        let footer = cx.debug_bounds("path-footer").expect("footer");
        let panel = cx.debug_bounds("modal-sheet").expect("path panel");
        assert!(
            notice.bottom() <= footer.top(),
            "notice sits above the footer"
        );
        assert!(notice.top() >= panel.top() && footer.bottom() <= panel.bottom());
        let shown = cx
            .debug_bounds(selector("path-notice", text))
            .unwrap_or_else(|| panic!("{text:?} reads in the results"));
        assert!(shown.top() >= notice.top() && shown.bottom() <= notice.bottom());
        assert!(
            cx.debug_bounds(selector("path-status", text)).is_none(),
            "{text:?} is not repeated in the footer"
        );
    }
    // Listed rows keep paging status in the footer.
    desk.update(cx, |desk, cx| {
        desk.picker = Some(crate::connection::path::PickerState {
            rows: vec![crate::connection::path::PathRow {
                path: "/repo/a".into(),
                kind: "file".into(),
            }],
            message: "Results truncated; narrow the search.".into(),
            ..path_picker()
        });
        cx.notify();
    });
    render(cx);
    let status = cx
        .debug_bounds("path-status: Results truncated; narrow the search.")
        .expect("truncation reads in the footer beside listed rows");
    let footer = cx.debug_bounds("path-footer").expect("footer");
    assert!(status.top() >= footer.top() && status.bottom() <= footer.bottom());
    assert!(cx.debug_bounds("path-results-notice").is_none());
}

#[gpui::test]
fn machines_are_reachable_from_the_palette_and_the_main_menu(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let connections = Some(crate::app::settings_panel::Page::Connections);

    keys(cx, "cmd-shift-p");
    type_text(cx, "add machine");
    assert_eq!(selected_command(&desk, cx), Some("add-host"));
    keys(cx, "enter");
    assert_eq!(modal(&desk, cx), Modal::AddHost);
    keys(cx, "escape");

    keys(cx, "cmd-shift-p");
    type_text(cx, "machines…");
    assert_eq!(selected_command(&desk, cx), Some("machines"));
    keys(cx, "enter");
    assert_eq!(modal(&desk, cx), Modal::None);
    assert_eq!(
        Some(
            crate::app::settings_panel::window_tests::opened(&desk, cx)
                .1
                .read_with(cx, |settings, _| settings.page)
        ),
        connections
    );
    let settings = crate::app::settings_panel::window_tests::opened(&desk, cx).0;
    settings
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    render(cx);
    assert_eq!(modal(&desk, cx), Modal::None);

    // The main menu's header leads to every machine, then to their settings.
    click(cx, "main-menu-rail");
    let menu = desk
        .read_with(cx, |desk, _| desk.menu_window())
        .expect("the main menu drops");
    let menu_cx = &mut VisualTestContext::from_window(menu, cx);
    for selector in ["glass-menu-header", "glass-menu-machines"] {
        let row = menu_cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("no {selector} in the menu"));
        menu_cx.simulate_click(row.center(), gpui::Modifiers::none());
        menu_cx.run_until_parked();
    }
    assert_eq!(modal(&desk, menu_cx), Modal::None);
    assert_eq!(
        Some(
            crate::app::settings_panel::window_tests::opened(&desk, menu_cx)
                .1
                .read_with(menu_cx, |settings, _| settings.page)
        ),
        connections
    );
}
