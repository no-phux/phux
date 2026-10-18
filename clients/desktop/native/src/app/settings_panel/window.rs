//! The Window and Advanced pages.

use super::*;

pub(super) fn window_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let sidebar_chord = desk
        .shown_chord("sidebar")
        .map(|chord| format!("Toggle it with {chord}."))
        .unwrap_or_default();
    let tabs = handle.clone();
    let tab_position = SettingField::render(move |_, _, cx| {
        let position = prefs_of(&tabs, cx).tab_position;
        segmented(
            &look_of(&tabs, cx),
            position,
            [
                (settings::TabPosition::Top, "Top", Icons::LayoutDashboard),
                (settings::TabPosition::Side, "Sidebar", Icons::PanelLeft),
            ]
            .map(|(value, label, icon)| Segment {
                value,
                label,
                icon,
                selector: format!("tab-position-{}", value.as_str()),
            }),
            |value| click_sets(&tabs, move |prefs| prefs.tab_position = value),
        )
    });
    let groups = handle.clone();
    let sidebar_grouping = SettingField::render(move |_, _, cx| {
        let grouping = prefs_of(&groups, cx).sidebar_grouping;
        segmented(
            &look_of(&groups, cx),
            grouping,
            [
                (settings::SidebarGrouping::Project, "Project", Icons::Folder),
                (settings::SidebarGrouping::Machine, "Machine", Icons::Server),
                (settings::SidebarGrouping::None, "None", Icons::ListTree),
            ]
            .map(|(value, label, icon)| Segment {
                value,
                label,
                icon,
                selector: format!("sidebar-grouping-{}", value.as_str()),
            }),
            |value| click_sets(&groups, move |prefs| prefs.sidebar_grouping = value),
        )
    });
    let (blur, solid) = (
        settings::WindowBackground::Blur,
        settings::WindowBackground::Solid,
    );
    let (blur_reader, blur_writer) = (handle.clone(), handle.clone());
    SettingPage::new("Layout & notifications")
        .icon(IconName::PanelLeft)
        .group(
            SettingGroup::new()
                .title("Layout")
                .item(
                    SettingItem::new(
                        "Tab placement",
                        resets(tab_position, handle, |prefs| &mut prefs.tab_position),
                    )
                    .description("Keep tabs above your work, or in the sidebar under their session."),
                )
                .item(
                    SettingItem::new(
                        "Group sessions by",
                        resets(sidebar_grouping, handle, |prefs| {
                            &mut prefs.sidebar_grouping
                        }),
                    )
                    .description("The same sessions, under projects, machines, or one list."),
                )
                .item(toggle(
                    handle,
                    "Show sidebar",
                    |prefs| &mut prefs.sidebar_visible,
                    sidebar_chord,
                ))
                .item(
                    SettingItem::new(
                        "Blur behind window",
                        SettingField::switch(
                            move |cx| prefs_of(&blur_reader, cx).window_background == blur,
                            move |value, cx| {
                                set_prefs(&blur_writer, cx, |prefs| {
                                    prefs.window_background = if value { blur } else { solid };
                                })
                            },
                        )
                        .default_value(settings::default_display().window_background == blur),
                    )
                    .description(
                        "Off paints an opaque window, which costs the compositor less on every frame.",
                    ),
                )
                .items(
                    (desk.prefs.window_background == blur)
                        .then(|| {
                            slider_item(
                                desk,
                                handle,
                                Knob::GLASS,
                                "Terminal background over the blurred desktop.",
                            )
                        })
                        .flatten(),
                ),
        )
        .group(SettingGroup::new().title("Notifications").item(toggle(
            handle,
            "Agent notifications",
            |prefs| &mut prefs.notifications,
            "Alert when an agent needs you. Also appears in Notification Center.",
        )))
}

pub(super) fn advanced_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let spacing = SettingGroup::new()
        .title("Terminal spacing")
        .items(slider_item(
            desk,
            handle,
            Knob::LINE_HEIGHT,
            line_height_description(desk),
        ))
        .items(slider_item(
            desk,
            handle,
            Knob::CELL_WIDTH,
            sourced(
                desk,
                "Column width as a percent of the font's advance.",
                &["adjust-cell-width"],
                |prefs| prefs.cell_width.to_string(),
            ),
        ))
        .item(stepper(
            handle,
            "Horizontal padding",
            (0., 64., 1.),
            |prefs| &mut prefs.padding_x,
            sourced(
                desk,
                "Points between the pane edge and the text.",
                &["window-padding-x"],
                |prefs| prefs.padding_x.to_string(),
            ),
        ))
        .item(stepper(
            handle,
            "Vertical padding",
            (0., 64., 1.),
            |prefs| &mut prefs.padding_y,
            sourced(
                desk,
                "Points above and below the text.",
                &["window-padding-y"],
                |prefs| prefs.padding_y.to_string(),
            ),
        ));
    SettingPage::new("Terminal spacing")
        .icon(IconName::Frame)
        .group(spacing)
        .group(SettingGroup::new().title("Pane dimming").items(slider_item(
            desk,
            handle,
            Knob::UNFOCUSED,
            sourced(
                desk,
                "Visibility of panes you are not typing in. 100% disables dimming.",
                &["unfocused-split-opacity"],
                |prefs| prefs.unfocused_opacity.to_string(),
            ),
        )))
}
