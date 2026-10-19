//! Work navigation: details, chord hints and destination pickers.

use super::*;

#[gpui::test]
fn fleet_command_is_disclosed_only_after_the_in_app_opt_in(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    type_text(cx, "Agent Fleet");
    assert_ne!(selected_command(&desk, cx), Some("agent-fleet"));
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.agent_fleet = true, cx)
    });
    render(cx);
    assert_eq!(selected_command(&desk, cx), Some("agent-fleet"));
    keys(cx, "enter");
    assert_eq!(modal(&desk, cx), Modal::AgentFleet);
    keys(cx, "escape");
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.agent_fleet = false, cx)
    });
    keys(cx, "cmd-shift-p");
    type_text(cx, "Agent Fleet");
    assert_ne!(selected_command(&desk, cx), Some("agent-fleet"));
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
fn a_remapped_chord_is_the_hint_the_palette_shows(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, cx| {
        desk.install_ghostty_for_test("keybind = cmd+alt+z=toggle_split_zoom\n", cx)
    });
    render(cx);
    let remapped = crate::settings::display_chord("cmd+alt+z");
    let default = crate::settings::display_chord("cmd+shift+enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.shown_chord("zoom")),
        Some(remapped.clone())
    );
    keys(cx, "cmd-shift-p");
    type_text(cx, "zoom pane");
    assert_eq!(selected_command(&desk, cx), Some("zoom"));
    // Debug selectors are looked up by `'static` name.
    let selector = |chord: &str| -> &'static str {
        Box::leak(format!("palette-chord:{chord}").into_boxed_str())
    };
    assert!(
        cx.debug_bounds(selector(&remapped)).is_some(),
        "the highlighted row shows the installed chord"
    );
    assert!(
        cx.debug_bounds(selector(&default)).is_none(),
        "the built-in default is not shown once remapped"
    );
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
fn moving_the_highlight_never_resizes_the_palette(cx: &mut TestAppContext) {
    use super::super::goto_facts::{Fact, FactsCard};
    use super::super::{Row, Section, Target};
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-p");
    let row = |title: &str, id: u32, preview: bool| Row {
        title: title.into(),
        detail: Some("~/project".into()),
        keywords: "project".into(),
        target: Target::Pane(phux_protocol::ResourceId::local(id)),
        command: None,
        category: "Terminals",
        chord: None,
        agent: None,
        preview: preview.then(|| FactsCard {
            title: title.into(),
            facts: vec![Fact::new("Directory", "~/project")],
        }),
    };
    desk.update(cx, |desk, cx| {
        let revision = super::super::search_revision(desk);
        desk.goto_search.rebuild(
            revision,
            vec![Section {
                label: None,
                rows: vec![row("Worker", 1, true), row("Scratch", 2, false)],
            }],
        );
        cx.notify();
    });
    render(cx);
    click(cx, "palette-details");
    let layout = |cx: &mut VisualTestContext| {
        [
            "palette-panel",
            "palette-filter",
            "palette-details",
            "palette-facts",
        ]
        .map(|selector| cx.debug_bounds(selector).expect(selector))
    };
    let first = layout(cx);
    keys(cx, "down");
    assert_eq!(
        layout(cx),
        first,
        "a row without details keeps the sheet and its controls"
    );
    keys(cx, "up");
    assert_eq!(layout(cx), first);
}
