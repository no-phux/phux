//! The titlebar: traffic lights, then the main menu trigger naming the
//! session behind a mark for its machine, the sidebar toggle,
//! tabs when they live up here, and a short cluster of actions.
//!
//! The full bar holds all of it; a slim strip, as the terminal skin wears
//! it, keeps the traffic lights, a centred title, tabs only when there are
//! several, and the skin toggle on hover. Controls fade with the bar's
//! height, so a skin shift morphs between the two.

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyElement, Context, FontWeight, IntoElement, MouseButton, Window, div, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Icon, Sizable};

use crate::settings::TabPosition;

use super::super::color::from_hex;
use super::super::desk::{self, Desk};
use super::super::machine_name;
use super::super::menus;
use super::super::notifications::{Kind, Notification};
use super::super::skin::Look;
use super::super::ui::{ghost_icon, icon_button};
use super::{navigator, tabs, tabs_at};

/// Left inset that clears the traffic lights.
pub(super) const TRAFFIC_LIGHTS: f32 = 78.;
/// Room for the main menu and sidebar toggle without a sidebar to size them.
pub(super) const MENU_RAIL: f32 = 256.;
/// Titlebar heights between which its controls fade in: a slim drag strip
/// holds only the traffic lights and a title.
const SLIM_BAR: f32 = 38.;
const FULL_BAR: f32 = 42.;

/// How much of the full titlebar's controls fit a bar `titlebar` points
/// tall: 0 in a slim strip, 1 in the full bar.
pub(super) fn controls_fit(titlebar: f32) -> f32 {
    ((titlebar - SLIM_BAR) / (FULL_BAR - SLIM_BAR)).clamp(0., 1.)
}

/// The lead's width: the sidebar's, so top tabs start where the panes do.
/// With the sidebar hidden it holds that width up to the menu rail, so at
/// the usual widths its toggle never moves out from under the pointer that
/// pressed it, and a wide sidebar never crowds the tabs it left behind.
fn titlebar_lead(quick: bool, sidebar: bool, sidebar_width: f32) -> f32 {
    if quick {
        return MENU_RAIL - 28.;
    }
    if sidebar {
        sidebar_width
    } else {
        sidebar_width.min(MENU_RAIL)
    }
}

pub(super) fn build(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    titlebar(desk, &look, cx).into_any_element()
}

fn titlebar(desk: &mut Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let region = desk
        .drag_region
        .get_or_insert_with(|| cx.new(crate::chrome::DragRegion::new))
        .clone();
    let skin = &look.skin;
    let height = skin.chrome.titlebar;
    let fit = controls_fit(height);
    let quick = super::quick(desk);
    let sidebar = navigator::wanted(desk);
    let presence = skin.chrome.sidebar.clamp(0., 1.);
    let full = titlebar_lead(quick, sidebar, desk.prefs.sidebar_width as f32 * presence);
    let lead = (TRAFFIC_LIGHTS + (full - TRAFFIC_LIGHTS) * fit).max(TRAFFIC_LIGHTS);
    let top_tabs = tabs_at(desk, skin, TabPosition::Top);
    let title = (fit < 1. && !top_tabs).then(|| {
        let title = desk
            .active_tab()
            .map(|tab| desk::tab_title(desk, tab))
            .unwrap_or_else(|| "phux".to_owned());
        match desk.remote.as_deref() {
            Some(host) => format!("{title} · {}", machine_name::display(host)),
            None => title,
        }
    });
    div()
        .id("titlebar")
        .group("titlebar")
        .h(px(height))
        .w_full()
        .relative()
        .flex()
        .items_center()
        .flex_shrink_0()
        .when(fit < 1., |bar| bar.overflow_hidden())
        .child(div().absolute().inset_0().child(region))
        .children(title.map(|title| {
            // Centred on the window with no hitbox, so the strip still drags.
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .px(px(TRAFFIC_LIGHTS + 8.))
                .when(fit > 0., |row| row.opacity(1. - fit))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(from_hex(&look.chrome.subtext))
                        .child(title),
                )
        }))
        .child(
            div()
                .w(px(lead))
                .h_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(2.))
                .pl(px(TRAFFIC_LIGHTS))
                .pr(px(8.))
                .when(fit < 1., |lead| lead.overflow_hidden().opacity(fit))
                .when(fit > 0., |lead| {
                    lead.child(main_menu(desk, look, cx))
                        // The trigger hugs its name; the buttons stay at the rail's end.
                        .child(div().flex_1())
                        .when(!quick, |lead| {
                            lead.child(icon_button(
                                "sidebar",
                                IconName::PanelLeft,
                                if sidebar {
                                    "Hide Sidebar"
                                } else {
                                    "Show Sidebar"
                                },
                                desk.shown_chord("sidebar"),
                                cx.listener(|desk, _, window, cx| {
                                    desk.execute("sidebar", window, cx)
                                }),
                            ))
                        })
                        .child(icon_button(
                            "new-tab",
                            IconName::Plus,
                            "New Terminal",
                            desk.shown_chord("new"),
                            cx.listener(|desk, _, window, cx| desk.execute("new", window, cx)),
                        ))
                })
                .with_spring(
                    "main-menu-rail",
                    gpui::SpringAnimation::new(super::CHROME_SPRING).to(lead),
                    |rail, width| rail.w(px(width)),
                ),
        )
        .when(top_tabs, |bar| {
            bar.child(tabs::strip(desk, TabPosition::Top, look, cx))
        })
        .when(!top_tabs, |bar| bar.child(div().flex_1()))
        .child(actions(desk, fit, look, cx))
}

/// The bell, the skin toggle and a ready update.
fn actions(desk: &Desk, fit: f32, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(2.))
        .pr(px(8.))
        .children(super::status::status_chips(desk, look, cx))
        .when(fit > 0., |cluster| {
            cluster.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .when(fit < 1., |actions| actions.opacity(fit))
                    .when(
                        desk.role != super::super::desk::WindowRole::Quick,
                        |actions| {
                            actions.child(icon_button(
                                "changes",
                                IconName::GitBranch,
                                if desk.changes.open {
                                    "Hide Changes"
                                } else {
                                    "Show Changes"
                                },
                                desk.shown_chord("changes"),
                                cx.listener(|desk, _, window, cx| {
                                    desk.execute("changes", window, cx)
                                }),
                            ))
                        },
                    )
                    .when(desk.native_agents_offered(), |actions| {
                        actions.child(native_agents_toggle(desk, cx))
                    })
                    .when(desk.prefs.agent_fleet, |actions| {
                        actions.child(div().debug_selector(|| "agent-fleet".into()).child(
                            icon_button(
                                "agent-fleet",
                                IconName::LayoutDashboard,
                                "Agent Fleet",
                                None,
                                cx.listener(|desk, _, window, cx| {
                                    desk.execute("agent-fleet", window, cx)
                                }),
                            ),
                        ))
                    })
                    .child(bell(desk, look, cx)),
            )
        })
        .child(
            // The way back stays reachable in the slim strip, where it
            // shows while the pointer is over the bar.
            div()
                .debug_selector(|| "skin-toggle".into())
                .when(fit == 0., |toggle| {
                    toggle
                        .invisible()
                        .group_hover("titlebar", |style| style.visible())
                })
                .child(icon_button(
                    "plain-terminal",
                    IconName::SquareTerminal,
                    // Named for the skin it switches to.
                    if desk.prefs.plain {
                        "Liquid Glass"
                    } else {
                        "Plain terminal"
                    },
                    desk.shown_chord("plain-terminal"),
                    cx.listener(|desk, _, window, cx| desk.execute("plain-terminal", window, cx)),
                )),
        )
        .children(update_button(desk, cx))
}

/// Snap every agent pane to its conversation or back to its terminal; lit
/// while the window shows conversations.
fn native_agents_toggle(desk: &Desk, cx: &mut Context<Desk>) -> impl IntoElement {
    let on = desk.prefs.native_agents;
    let label = if on {
        "Show Agent Terminals"
    } else {
        "Show Agent Conversations"
    };
    let tip = match desk.shown_chord("native-agents") {
        Some(chord) => format!("{label}  {chord}"),
        None => label.to_owned(),
    };
    div()
        .debug_selector(|| "native-agents-toggle".into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            ghost_icon(
                "native-agents",
                if on {
                    IconName::MessagesSquare
                } else {
                    IconName::SquareTerminal
                },
                label,
            )
            .small()
            .toggled(on)
            .tooltip(tip)
            .on_click(cx.listener(|desk, _, window, cx| desk.execute("native-agents", window, cx))),
        )
}

/// The main menu trigger: the session this window shows, led by a small
/// mark for the machine it runs on, and a chevron. It opens the glass main
/// menu, whose header switches sessions on any machine.
fn main_menu(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let session = if desk.session.is_empty() {
        "phux".to_owned()
    } else {
        desk.session.clone()
    };
    let open = desk.menu_open(menus::MAIN);
    let hover = look.hover;
    let tip = format!(
        "{session} on {}",
        match desk.remote.as_deref() {
            Some(host) => machine_name::display(host),
            None => "this Mac".to_owned(),
        }
    );
    let muted = from_hex(&look.chrome.muted);
    div()
        .id("main-menu")
        .debug_selector(|| "main-menu-rail".into())
        .relative()
        .min_w_0()
        .max_w(px(196.))
        .h(px(tabs::tab_height(look.skin.chrome.titlebar)))
        .px(px(9.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded_full()
        .occlude()
        .cursor_pointer()
        .when(open, |trigger| trigger.bg(look.active))
        .hover(move |style| style.bg(hover))
        .role(gpui::Role::Button)
        .aria_label(format!("Main menu, {tip}"))
        .child(desk.anchors.probe(menus::MAIN))
        .child(
            div()
                .debug_selector(|| "main-menu-machine".into())
                .flex_shrink_0()
                .child(
                    Icon::new(menus::machine_icon(desk.remote.as_deref()))
                        .size(px(12.))
                        .text_color(muted),
                ),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(look.ink)
                .child(session),
        )
        .child(
            Icon::new(IconName::ChevronDown)
                .size(px(12.))
                .flex_shrink_0()
                .text_color(muted),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|desk, _, window, cx| desk.open_main_menu(window, cx)))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
        .into_any_element()
}

fn update_button(desk: &Desk, cx: &mut Context<Desk>) -> Option<AnyElement> {
    let entity = desk.updates.clone();
    let action = entity.read(cx).action()?;
    Some(
        Button::new("desktop-update-ready")
            .label(action.label())
            .small()
            .on_click(move |_, _, cx| entity.update(cx, |updater, cx| updater.act(cx)))
            .into_any_element(),
    )
}

/// The titlebar's way into the notification list, with the unread count.
fn bell(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let unread: Vec<&Notification> = desk.feed.unread().collect();
    let badge = unread.first().map(|_| {
        let tint = unread
            .iter()
            .find(|entry| entry.kind == Kind::Error)
            .or(unread.last())
            .map_or_else(
                || from_hex(&look.chrome.theme.accent),
                |entry| super::notification_edge(entry, look),
            );
        let count = if unread.len() > 9 {
            "9+".to_owned()
        } else {
            unread.len().to_string()
        };
        div()
            .absolute()
            .top(px(1.))
            .right(px(0.))
            .min_w(px(14.))
            .h(px(14.))
            .px(px(3.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(tint)
            .text_color(from_hex(&look.chrome.theme.background))
            .text_size(px(9.))
            .font_weight(FontWeight::BOLD)
            .child(count)
    });
    div()
        .relative()
        .debug_selector(|| "notifications".into())
        .child(desk.anchors.probe("notifications"))
        .child(icon_button(
            "notifications",
            IconName::Bell,
            "Inbox & notifications · badge counts unread alerts",
            desk.shown_chord("notifications"),
            cx.listener(|desk, _, window, cx| desk.execute("notifications", window, cx)),
        ))
        .children(badge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_holds_the_sidebar_width_with_or_without_the_sidebar() {
        assert_eq!(titlebar_lead(false, true, 248.), 248.);
        assert_eq!(titlebar_lead(false, false, 248.), 248.);
        assert_eq!(titlebar_lead(false, true, 400.), 400.);
        assert_eq!(titlebar_lead(false, false, 400.), MENU_RAIL);
        assert!(titlebar_lead(true, true, 248.) > TRAFFIC_LIGHTS);
    }
}
