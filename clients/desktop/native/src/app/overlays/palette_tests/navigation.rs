use super::*;

#[gpui::test]
fn work_search_discovers_a_preference_and_settings_keep_one_native_hub(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-p");
    type_text(cx, "line height");
    keys(cx, "enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        Some(crate::app::settings_panel::Page::Spacing)
    );
    assert!(cx.debug_bounds("settings-panel").is_some());
    click(cx, "palette-mode-commands");
    assert_eq!(modal(&desk, cx), Modal::Commands);
    keys(cx, "escape");
    assert_desk_focus(&desk, cx);
    keys(cx, "cmd-,");
    type_text(cx, "remote");
    keys(cx, "enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        Some(crate::app::settings_panel::Page::Connections)
    );
    keys(cx, "escape");
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn navigation_details_are_explicit_and_preserve_search_focus(cx: &mut TestAppContext) {
    use super::super::{Row, Section, Target};
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(gpui::px(320.), gpui::px(280.)));
    keys(cx, "cmd-p");
    desk.update(cx, |desk, cx| {
        let revision = super::super::search_revision(desk);
        desk.goto_search.rebuild(
            revision,
            vec![Section {
                label: None,
                rows: vec![Row {
                    title: "Worker".into(),
                    detail: Some("~/project".into()),
                    keywords: "project".into(),
                    target: Target::Pane(phux_protocol::ResourceId::local(1)),
                    command: None,
                    category: "Terminals",
                    chord: None,
                    agent: None,
                    preview: Some(super::super::goto_facts::FactsCard {
                        title: "Worker".into(),
                        facts: vec![super::super::goto_facts::Fact::new(
                            "Directory",
                            "~/project",
                        )],
                    }),
                }],
            }],
        );
        cx.notify();
    });
    render(cx);
    assert!(cx.debug_bounds("palette-facts").is_none());
    let panel = cx.debug_bounds("palette-panel").unwrap();
    let modes = cx.debug_bounds("palette-mode-settings").unwrap();
    let filter = cx.debug_bounds("palette-filter").unwrap();
    let details_bounds = cx.debug_bounds("palette-details").unwrap();
    for bounds in [modes, filter, details_bounds] {
        assert!(
            bounds.left() >= panel.left() && bounds.right() <= panel.right(),
            "header controls stay inside the panel"
        );
    }
    assert!(
        modes.bottom() <= filter.top(),
        "compact controls occupy their own row"
    );
    assert!(
        filter.right() <= details_bounds.left(),
        "filter and details do not overlap"
    );
    click(cx, "palette-details");
    assert!(cx.debug_bounds("palette-facts").is_some());
    let panel = cx.debug_bounds("palette-panel").unwrap();
    assert!(panel.origin.x >= gpui::px(0.) && panel.right() <= gpui::px(320.));
    assert!(
        panel.bottom() <= gpui::px(280.),
        "details stay on a small screen"
    );
    type_text(cx, "project");
    assert_eq!(
        query(&desk, cx),
        "project",
        "details keep the search keyboard"
    );
    keys(cx, "escape escape cmd-p");
    assert!(!desk.read_with(cx, |desk, _| desk.palette_view.details));
}

#[gpui::test]
fn destination_pickers_drop_the_global_strip_and_keep_every_route(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    for modal in [Modal::WindowSession, Modal::MoveResources] {
        open(&desk, &modal, cx);
        assert!(cx.debug_bounds("palette-panel").is_some());
        for global in [
            "palette-mode-goto",
            "palette-mode-commands",
            "palette-mode-settings",
            "palette-filter",
        ] {
            assert!(
                cx.debug_bounds(global).is_none(),
                "{modal:?} is scoped to its destination, not the {global} hub control"
            );
        }
        keys(cx, "escape");
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::None
        );
    }
    // The navigator still reaches every route.
    keys(cx, "cmd-p");
    for route in [
        "palette-mode-goto",
        "palette-mode-commands",
        "palette-mode-settings",
        "palette-filter",
    ] {
        assert!(cx.debug_bounds(route).is_some(), "{route} stays reachable");
    }
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
    assert_eq!(modal(&desk, cx), Modal::Settings);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        connections
    );
    keys(cx, "escape");
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
    assert_eq!(modal(&desk, menu_cx), Modal::Settings);
    assert_eq!(
        desk.read_with(menu_cx, |desk, _| desk.settings_page),
        connections
    );
}
