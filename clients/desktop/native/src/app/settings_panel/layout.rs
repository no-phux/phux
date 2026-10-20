//! Window layout, notifications, terminal spacing, and pane dimming.

use super::catalog::{Content as SettingPage, Group as SettingGroup, Item as SettingItem};
use gpui::WeakEntity;
use gpui_kit::assets::IconName as Icons;
use gpui_kit::component::IconName;
use gpui_kit::component::setting::SettingField;

use crate::settings;

use super::super::desk::Desk;
use super::super::ui::segmented::{Segment, segmented};
use super::{
    Knob, click_sets, look_of, notes, prefs_of, resets, set_prefs, slider_item, sourced, stepper,
    toggle,
};

/// Line height's source. A Ghostty import resets it to 1 for a font family,
/// size or cell height, and the cell height (which has no slider) is named here.
fn line_height_description(desk: &Desk) -> String {
    let line = sourced(
        desk,
        "Row height as a multiple of the font size.",
        &["adjust-cell-height", "font-size", "font-family"],
        |prefs| prefs.line_height.to_string(),
    );
    if (desk.prefs.cell_height - 1.0).abs() < f64::EPSILON {
        return line;
    }
    let source = settings::display_provenance(
        &desk.prefs,
        desk.ghostty(),
        &["adjust-cell-height"],
        |prefs| prefs.cell_height.to_string(),
    );
    let percent = ((desk.prefs.cell_height - 1.0) * 100.0).round();
    notes([
        line.as_str(),
        &format!("Cell height {percent:+}% ({})", source.label()),
    ])
}

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
                (
                    settings::TabPosition::Bottom,
                    "Bottom",
                    Icons::LayoutDashboard,
                ),
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
                    .description("Keep tabs above or below your work, or in the sidebar’s fixed tab area."),
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
                    SettingItem::switch(
                        "Blur behind window",
                            move |cx| prefs_of(&blur_reader, cx).window_background == blur,
                            move |value, cx| {
                                set_prefs(&blur_writer, cx, |prefs| {
                                    prefs.window_background = if value { blur } else { solid };
                                })
                            },
                        Some(settings::default_display().window_background == blur),
                    )
                    .description(
                        "Off paints an opaque window, which costs the compositor less on every frame.",
                    ),
                )
                .items(slider_item(
                    desk,
                    handle,
                    Knob::GLASS,
                    "Terminal background opacity. Applies when Blur behind window is on.",
                )),
        )
        .group(sidebar_hosts(desk, handle))
        .group(SettingGroup::new().title("Experimental").item(toggle(
            handle,
            "Agent fleet",
            |prefs| &mut prefs.agent_fleet,
            "A live tiled overview of agents across your machines. Open from the titlebar or Agent Fleet command. Motion follows Appearance > Animations and system Reduce Motion.",
        )))
        .group(SettingGroup::new().title("Notifications").item(toggle(
            handle,
            "Agent notifications",
            |prefs| &mut prefs.notifications,
            "Alert when an agent needs you. Also appears in Notification Center.",
        )))
}

fn sidebar_hosts(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingGroup {
    let mut hosts = vec![None];
    hosts.extend(desk.hosts.iter().map(|host| Some(host.name.clone())));
    if !hosts.contains(&desk.remote) {
        hosts.push(desk.remote.clone());
    }
    SettingGroup::new()
        .title("Sidebar machines")
        .items(hosts.into_iter().map(|host| {
            let label = super::super::machine_name::label(host.as_deref());
            let (reader, writer) = (handle.clone(), handle.clone());
            let read_host = host.clone();
            SettingItem::switch(
                label,
                move |cx| prefs_of(&reader, cx).sidebar_shows_host(read_host.as_deref()),
                move |visible, cx| {
                    set_prefs(&writer, cx, |prefs| {
                        prefs.set_sidebar_host_visible(host.clone(), visible)
                    })
                },
                Some(true),
            )
            .description(
                "Show this machine in the sidebar. Selecting a session never changes this filter.",
            )
        }))
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
