//! Terminal-first chrome: titlebar, sidebar, split tree, status, overlays.
//!
//! The window is one sheet of glass over the desktop blur. Titlebar, sidebar
//! and status sit on it unframed; panes float on it as rounded cards with no
//! header. A pane's controls appear when the pointer reaches its top edge.
//! Every extent, radius and paint comes from the desk's skin each frame, so
//! a skin shift (such as into the plain terminal) morphs instead of popping.
//! The titlebar and sidebar are cached views: a terminal frame marks the desk
//! dirty as their ancestor, and they stay as painted until the desk notifies.

use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyElement, AnyView, App, BoxShadow, ClickEvent, Context, CursorStyle,
    DispatchPhase, DragMoveEvent, Entity, ExternalPaths, FileDropEvent, Hsla, IntoElement,
    Keystroke, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, SharedString,
    StyleRefinement, Styled, WeakEntity, Window, canvas, div, linear_color_stop, linear_gradient,
    point, px, relative,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use phux_client_runtime::control::{PaneDescriptor, Status};
use phux_protocol::ids::ResourceKey;
use phux_protocol::wire::frame::{ExecState, ResourceRecord};
use phux_protocol::{ResourceId, ResourceKind};

use crate::connection::{AgentInfo, AgentState, state_rank};
use crate::settings::{Skin, TabPosition, TabsWhen};
use crate::workspace::layout::{self, Axis, DeskTab, LayoutNode, Placement};

use super::actions::{OpenPathDirectory, Run};
use super::color::from_hex;
use super::desk::{self, Desk, Modal, Tone};
use super::keys::gpui_stroke;
use super::motion;
use super::notifications::{Kind, Notification};
use super::overlays;
use super::skin::{Look, Surface, Surfaced};

const SIDEBAR_ROW: f32 = 32.;
const SIDEBAR_ICON: f32 = 16.;
/// How far down a pane the pointer reveals its controls.
const CONTROLS_EDGE: f32 = 36.;
/// Left inset that clears the traffic lights.
const TRAFFIC_LIGHTS: f32 = 78.;
/// Room for the search menu and sidebar toggle, even with the navigator hidden.
const MENU_RAIL: f32 = 224.;
const TAB_RAIL: f32 = 212.;
/// Titlebar heights between which its controls fade in: a slim drag strip
/// holds only the traffic lights and a title.
const SLIM_BAR: f32 = 30.;
const FULL_BAR: f32 = 42.;
/// The narrowest grab area a split keeps when its painted gap is thinner.
const SPLIT_GRAB: f32 = 8.;

/// How much of the full titlebar's controls fit a bar `titlebar` points
/// tall: 0 in a slim strip, 1 in the full bar.
fn controls_fit(titlebar: f32) -> f32 {
    ((titlebar - SLIM_BAR) / (FULL_BAR - SLIM_BAR)).clamp(0., 1.)
}

/// Titlebar tab height for a bar `titlebar` points tall.
fn tab_height(titlebar: f32) -> f32 {
    (titlebar - 14.).clamp(22., 30.)
}

/// Whether the skin shows workspace tabs when there are `count` of them.
fn tabs_shown(skin: &Skin, count: usize) -> bool {
    skin.chrome.tabs == TabsWhen::Always || count >= 2
}

/// Whether the desk's workspace tabs show at `position` in `skin`.
fn tabs_at(desk: &Desk, skin: &Skin, position: TabPosition) -> bool {
    let count = desk
        .link
        .as_ref()
        .map_or(0, |link| link.workspace.tabs.len());
    desk.tab_position() == position && tabs_shown(skin, count)
}

/// Whether the prefs show the sidebar in this window.
fn sidebar_wanted(desk: &Desk) -> bool {
    desk.prefs.sidebar_visible && desk.role != desk::WindowRole::Quick
}

/// How see-through the skin's pane cards are, 0 (solid) to 1. Gloss
/// highlights scale with it so they fade out as the glass goes opaque.
fn gloss(skin: &Skin) -> f32 {
    (1. - skin.material.card_solid).clamp(0., 1.)
}

/// Sidebar and list rows.
fn row_radius(skin: &Skin) -> f32 {
    (skin.shape.kit_radius - 2.).max(0.)
}

/// Small status chips.
fn chip_radius(skin: &Skin) -> f32 {
    skin.shape.kit_radius / 2.
}

fn shadow(color: Hsla, y: f32, blur: f32, spread: f32, inset: bool) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset,
    }
}

/// The selected tab's lift, plus a specular top edge and a faint accent glow
/// while the skin is glassy.
fn tab_glow(look: &Look) -> Vec<BoxShadow> {
    let mut shadows = look.tab_shadow.clone();
    let gloss = gloss(&look.skin);
    if gloss > 0. && look.lit.a > 0. {
        let accent = from_hex(&look.chrome.theme.accent);
        shadows.push(shadow(look.lit.opacity(0.5 * gloss), 1., 0., 0., true));
        shadows.push(shadow(accent.opacity(0.1 * gloss), 0., 12., -4., false));
    }
    shadows
}

/// A pane card's shadow plus a faint inner rim while the skin is glassy.
fn pane_shadows(look: &Look) -> Vec<BoxShadow> {
    let mut shadows = look.pane_shadow.clone();
    let rim = look.rim.opacity(0.6 * gloss(&look.skin));
    if rim.a > 0. {
        shadows.push(shadow(rim, 0., 0., 1., true));
    }
    shadows
}

/// Light catching a card's top edge: a hairline brightest at the centre,
/// `inset` points in from either corner. None when `color` is clear.
fn specular(color: Hsla, inset: f32) -> Option<gpui::Div> {
    let half = |from: Hsla, to: Hsla| {
        div().flex_1().h_full().bg(linear_gradient(
            90.,
            linear_color_stop(from, 0.),
            linear_color_stop(to, 1.),
        ))
    };
    (color.a > 0.).then(|| {
        div()
            .absolute()
            .top_0()
            .left(px(inset))
            .right(px(inset))
            .h(px(1.))
            .flex()
            .child(half(color.opacity(0.), color))
            .child(half(color, color.opacity(0.)))
    })
}

/// Stable per route, independent of inventory order and the selected session.
fn host_tint(host: Option<&str>, look: &Look) -> Hsla {
    let hue = host.map_or(190., |name| {
        let hash = name.bytes().fold(2_166_136_261_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
        });
        (hash % 360) as f32
    });
    gpui::hsla(
        hue / 360.,
        0.58,
        if look.chrome.theme.appearance == "light" {
            0.38
        } else {
            0.72
        },
        1.,
    )
}

/// A round swatch `size` points across.
pub(super) fn dot(size: f32, color: Hsla) -> gpui::Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        .bg(color)
}

fn host_chip(host: Option<&str>, online: bool, look: &Look) -> impl IntoElement {
    let tint = host_tint(host, look);
    let detail = format!("Runs on {}", host.unwrap_or("Local"));
    div()
        .min_w_0()
        .max_w_full()
        .flex()
        .items_center()
        .gap(px(5.))
        .px(px(6.))
        .py(px(2.))
        .rounded_full()
        .border_1()
        .border_color(tint.opacity(0.18))
        .bg(linear_gradient(
            180.,
            linear_color_stop(tint.opacity(0.13), 0.),
            linear_color_stop(tint.opacity(0.05), 1.),
        ))
        .child(dot(5., tint))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(10.))
                .text_color(from_hex(&look.chrome.subtext))
                .child(detail),
        )
        .when(!online, |chip| {
            chip.child(muted(look, "· offline").flex_shrink_0().text_size(px(10.)))
        })
}

fn workspace_identity(desk: &Desk, look: &Look) -> impl IntoElement {
    let host = desk.remote.as_deref();
    let full = format!("{} · Runs on {}", desk.session, host.unwrap_or("Local"));
    div()
        .id("workspace-identity")
        .debug_selector(|| "workspace-identity".into())
        .w(px(180.))
        .min_w_0()
        .flex_shrink_0()
        .px(px(8.))
        .flex()
        .flex_col()
        .items_start()
        .gap(px(1.))
        .child(
            div()
                .w_full()
                .truncate()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(desk.session.clone()),
        )
        .child(host_chip(
            host,
            desk.link
                .as_ref()
                .is_some_and(|link| link.connection.status == Status::Attached),
            look,
        ))
        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
}
#[derive(Clone)]
struct ResourceDrag {
    keys: Vec<ResourceKey>,
    label: String,
}

impl gpui::Render for ResourceDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(12.))
            .py(px(8.))
            .rounded(px(6.))
            .bg(gpui::rgb(0x202730))
            .text_color(gpui::white())
            .text_sm()
            .child(self.label.clone())
    }
}

/// Chrome slots refresh on desk notifications, not on terminal-only frames.
#[derive(Default)]
pub(super) struct ChromeCache {
    titlebar: Option<Entity<CachedChrome>>,
    sidebar: Option<Entity<CachedChrome>>,
    tabs: Option<Entity<CachedChrome>>,
    overlays: Option<Entity<CachedChrome>>,
}

struct CachedChrome {
    desk: WeakEntity<Desk>,
    build: fn(&mut Desk, &mut Window, &mut Context<Desk>) -> AnyElement,
}

impl gpui::Render for CachedChrome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(desk) = self.desk.upgrade() else {
            return div().into_any_element();
        };
        desk.update(cx, |desk, cx| (self.build)(desk, window, cx))
    }
}

fn chrome_slot(
    slot: &mut Option<Entity<CachedChrome>>,
    build: fn(&mut Desk, &mut Window, &mut Context<Desk>) -> AnyElement,
    style: StyleRefinement,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    slot.get_or_insert_with(|| {
        let observed = cx.entity();
        cx.new(|cx| {
            cx.observe(&observed, |_, _, cx| cx.notify()).detach();
            CachedChrome {
                desk: observed.downgrade(),
                build,
            }
        })
    })
    .clone()
    .cached(style)
}

fn build_titlebar(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    titlebar(desk, &look, cx).into_any_element()
}

fn build_sidebar(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    div()
        .w(px(desk.prefs.sidebar_width as f32))
        .h_full()
        .flex_shrink_0()
        .relative()
        .flex()
        .flex_col()
        .child(sidebar_list(desk, &look, cx))
        .child(sidebar_resize(desk, &look, cx))
        .into_any_element()
}

fn build_tab_rail(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    div()
        .w(px(TAB_RAIL))
        .h_full()
        .min_h_0()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .px(px(8.))
        .pt(px(10.))
        .pb(px(8.))
        .child(
            muted(&look, "Workspace")
                .px(px(10.))
                .pb(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM),
        )
        .child(
            div()
                .id("side-tabs-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(tab_strip(desk, TabPosition::Side, &look, cx)),
        )
        .into_any_element()
}

fn build_overlays(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    overlays::layer(desk, window, cx)
}

pub fn render(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    let _frame = crate::diag::FRAME.timer();
    desk.set_viewport(window.viewport_size());
    desk.sync_title(window);
    desk.settle_focus(window, cx);
    desk.drive_skin(window, cx);
    desk.ensure_clock(cx);
    desk.flush_system(cx);
    // Read after the skin is driven, so a shift paints its current step.
    let look = desk.look();
    let skin = look.skin;
    if crate::diag::evidence_pending(cx) {
        let desk_handle = cx.weak_entity();
        crate::diag::arm_evidence(
            Box::new(move |app| {
                desk_handle
                    .upgrade()
                    .map(|desk| desk.read(app).frame_report(app))
            }),
            window,
            cx,
        );
    }
    let cursor = desk.drag_cursor();
    if !cx.has_active_drag() {
        desk.drop_target = None;
    }
    observe_motion(desk, window);
    // The sidebar slides shut as the skin withdraws it, whatever the prefs say.
    let presence = skin.chrome.sidebar.clamp(0., 1.);
    let sidebar = sidebar_wanted(desk) && presence > 0.;
    let sidebar_width = desk.prefs.sidebar_width as f32 * presence;
    let side_tabs = tabs_at(desk, &skin, TabPosition::Side);
    let sheen_height = skin.material.sheen_height;
    let sheen = sheen_height > 0.
        && look.sheen.a > 0.
        && (skin.chrome.sheen || desk.shift_progress().is_some());

    div()
        .id("desk")
        .key_context("Desk")
        .track_focus(&desk.focus)
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(look.sheet)
        .when(sheen, |sheet| {
            sheet.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(sheen_height))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(look.sheen, 0.),
                        linear_color_stop(look.sheen.opacity(0.), 1.),
                    )),
            )
        })
        .text_color(look.ink)
        .on_action(cx.listener(|desk, action: &Run, window, cx| {
            desk.execute(&action.id, window, cx);
        }))
        .on_action(cx.listener(|desk, _: &OpenPathDirectory, window, cx| {
            desk.open_selected_directory(window, cx);
        }))
        .on_key_down(cx.listener(|desk, event: &gpui::KeyDownEvent, window, cx| {
            desk.on_key(event, window, cx);
        }))
        .child(chrome_slot(
            &mut desk.chrome.titlebar,
            build_titlebar,
            StyleRefinement::default()
                .w_full()
                .h(px(skin.chrome.titlebar)),
            cx,
        ))
        .child(
            div()
                .flex_grow(1.)
                .flex()
                .min_h(px(0.))
                .when(sidebar, |row| {
                    let style = StyleRefinement::default().w(px(sidebar_width)).h_full();
                    let style = if presence < 1. {
                        style.overflow_hidden().opacity(presence)
                    } else {
                        style
                    };
                    row.child(chrome_slot(
                        &mut desk.chrome.sidebar,
                        build_sidebar,
                        style,
                        cx,
                    ))
                })
                .when(side_tabs, |row| {
                    row.child(chrome_slot(
                        &mut desk.chrome.tabs,
                        build_tab_rail,
                        StyleRefinement::default()
                            .w(px(TAB_RAIL))
                            .h_full()
                            .flex_shrink_0(),
                        cx,
                    ))
                })
                .child(main_column(desk, &look, window, cx)),
        )
        .child(chrome_slot(
            &mut desk.chrome.overlays,
            build_overlays,
            StyleRefinement::default().absolute().inset_0().size_full(),
            cx,
        ))
        .children(cursor.map(|cursor| drag_capture(cursor, cx)))
}

/// Feed this frame's placements to the motion clock, and keep frames coming
/// while a pane is still growing in or folding away.
fn observe_motion(desk: &mut Desk, window: &mut Window) {
    let now = Instant::now();
    let tab = desk
        .link
        .as_ref()
        .and_then(|link| link.workspace.active_tab());
    let placements = tab
        .map(|tab| {
            layout::placements(&tab.root)
                .into_iter()
                .map(|placement| placement.id.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    desk.motion
        .observe(tab.map(|tab| tab.id.as_str()), &placements, now);
    if desk.motion.active(now) {
        window.request_animation_frame();
    }
}

/// While a divider, the sidebar edge or a tab is dragged, follow the pointer
/// anywhere in the window, not only over the element that started the drag.
fn drag_capture(cursor: CursorStyle, cx: &mut Context<Desk>) -> AnyElement {
    let handle = cx.weak_entity();
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            window.set_window_cursor_style(cursor);
            let moves = handle.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    let (x, y) = (event.position.x.as_f32(), event.position.y.as_f32());
                    let pressed = event.pressed_button.is_some();
                    let _ = moves.update(cx, |desk, cx| desk.drag_move(x, y, pressed, cx));
                }
            });
            let ups = handle.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    let _ = ups.update(cx, |desk, cx| desk.end_drag(cx));
                }
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// The full bar holds the main menu, tabs and actions; a slim strip, as the
/// terminal skin wears it, keeps the traffic lights, a centred title, tabs
/// only when there are several, and the skin toggle on hover. Controls fade
/// with the bar's height, so a skin shift morphs between the two.
fn titlebar(desk: &mut Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let region = desk
        .drag_region
        .get_or_insert_with(|| cx.new(crate::chrome::DragRegion::new))
        .clone();
    let skin = &look.skin;
    let height = skin.chrome.titlebar;
    let fit = controls_fit(height);
    let quick = desk.role == desk::WindowRole::Quick;
    let sidebar = sidebar_wanted(desk);
    let presence = skin.chrome.sidebar.clamp(0., 1.);
    let full = titlebar_lead(
        sidebar && presence > 0.,
        quick,
        desk.prefs.sidebar_width as f32 * presence,
    );
    let lead = (TRAFFIC_LIGHTS + (full - TRAFFIC_LIGHTS) * fit).max(TRAFFIC_LIGHTS);
    let top_tabs = tabs_at(desk, skin, TabPosition::Top);
    let title = (fit < 1. && !top_tabs).then(|| {
        let title = desk
            .active_tab()
            .map(|tab| desk::tab_title(desk, tab))
            .unwrap_or_else(|| "phux".to_owned());
        match desk.remote.as_deref() {
            Some(host) => format!("{title} · {host}"),
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
                        .font_weight(gpui::FontWeight::MEDIUM)
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
                    lead.child(main_menu(look, cx)).when(!quick, |lead| {
                        lead.child(icon_button(
                            "sidebar",
                            IconName::PanelLeft,
                            if sidebar {
                                "Hide Sidebar"
                            } else {
                                "Show Sidebar"
                            },
                            desk.shown_chord("sidebar"),
                            cx.listener(|desk, _, window, cx| desk.execute("sidebar", window, cx)),
                        ))
                    })
                })
                .with_spring(
                    "main-menu-rail",
                    gpui::SpringAnimation::new(gpui::SpringConfig::new(600., 49., 1.)).to(lead),
                    |rail, width| rail.w(px(width)),
                ),
        )
        .when(fit > 0., |bar| {
            bar.child(
                div()
                    .flex_shrink_0()
                    .when(fit < 1., |slot| slot.overflow_hidden().opacity(fit))
                    .child(workspace_identity(desk, look)),
            )
        })
        .when(top_tabs, |bar| {
            bar.child(tab_strip(desk, TabPosition::Top, look, cx))
        })
        .when(!top_tabs, |bar| bar.child(div().flex_1()))
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(2.))
                .pr(px(8.))
                .when(fit > 0., |cluster| {
                    cluster.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .when(fit < 1., |actions| actions.opacity(fit))
                            .child(icon_button(
                                "launch-native-agent",
                                IconName::Bot,
                                "New agent",
                                None,
                                cx.listener(|desk, _, window, cx| {
                                    desk.execute("native-agent", window, cx)
                                }),
                            ))
                            .child(icon_button(
                                "new-tab",
                                IconName::Plus,
                                "New Terminal",
                                desk.shown_chord("new"),
                                cx.listener(|desk, _, window, cx| desk.execute("new", window, cx)),
                            ))
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
                            cx.listener(|desk, _, window, cx| {
                                desk.execute("plain-terminal", window, cx)
                            }),
                        )),
                )
                .child(update_button(desk, cx)),
        )
}

/// One tab renderer and one set of tab actions for both placements.
fn tab_strip(
    desk: &Desk,
    position: TabPosition,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let (tabs, active) = desk.link.as_ref().map_or((&[][..], ""), |link| {
        (
            link.workspace.tabs.as_slice(),
            link.workspace.active_id.as_str(),
        )
    });
    let geometry = desk.tab_geometry.clone();
    geometry.borrow_mut().prepare(position, tabs.len());
    let side = position == TabPosition::Side;
    div()
        .id(if side { "side-tabs" } else { "top-tabs" })
        .when(side, |list| list.w_full())
        .when(!side, |list| {
            list.flex_1().min_w_0().h_full().overflow_x_scroll()
        })
        .child(
            // Measure inside the scroller: its children then include the actual
            // painted scroll offset, not the unscrolled layout coordinates.
            div()
                .flex()
                .gap(px(5.))
                .when(side, |list| list.flex_col().w_full())
                .when(!side, |list| {
                    list.w_full()
                        .min_w(px(tabs.len() as f32 * 105. + 3.))
                        .h_full()
                        .items_center()
                        .px(px(4.))
                })
                .children(tabs.iter().map(|tab| {
                    tab_button(desk, tab, active, position, look, cx).into_any_element()
                }))
                .on_children_prepainted(move |bounds, _, _| {
                    let mut geometry = geometry.borrow_mut();
                    for (index, bounds) in bounds.into_iter().enumerate() {
                        geometry.record(index, position, bounds);
                    }
                }),
        )
}

fn update_button(desk: &Desk, cx: &mut Context<Desk>) -> AnyElement {
    let entity = desk.updates.clone();
    let Some(action) = entity.read(cx).desktop_action() else {
        return div().into_any_element();
    };
    Button::new("desktop-update-ready")
        .label(action.label())
        .small()
        .on_click(move |_, _, cx| entity.update(cx, |updater, cx| updater.desktop_act(cx)))
        .into_any_element()
}

fn tab_button(
    desk: &Desk,
    tab: &DeskTab,
    active: &str,
    position: TabPosition,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let height = tab_height(look.skin.chrome.titlebar);
    let id = &tab.id;
    let tab_selector = SharedString::from(format!("tab-{id}"));
    let close_selector = SharedString::from(format!("tab-close-{id}"));
    let selected = tab.id == active;
    let zoomed = tab.zoomed_id.is_some().then_some("⤢");
    let (hover, muted) = (look.hover, from_hex(&look.chrome.muted));
    let close_id = tab.id.clone();
    let drag_id = tab.id.clone();
    let rename_id = tab.id.clone();
    let middle_id = tab.id.clone();
    let side = position == TabPosition::Side;
    div()
        .id(tab_selector.clone())
        .debug_selector(move || tab_selector.to_string())
        .group("tab")
        .relative()
        .flex_shrink_0()
        .when(side, |tab| tab.h(px(38.)).w_full())
        .when(!side, |tab| {
            tab.h(px(height)).min_w(px(100.)).max_w(px(220.)).flex_1()
        })
        .pl(px(10.))
        .pr(px(4.))
        .flex()
        .items_center()
        .gap(px(6.))
        .when(selected, |tab| {
            tab.surface(look, if side { Surface::SideTab } else { Surface::Tab })
                .shadow(tab_glow(look))
        })
        .when(!selected, |tab| {
            tab.rounded(px(look.skin.shape.tab_radius[usize::from(side)]))
                .border_1()
                .border_color(gpui::transparent_black())
                .hover(move |style| style.bg(hover))
        })
        .block_mouse_except_scroll()
        .children(strongest_agent(desk, tab).map(|state| dot(7., from_hex(agent_swatch(state)))))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(selected, |label| {
                    label.font_weight(gpui::FontWeight::MEDIUM)
                })
                .text_color(if selected {
                    look.ink
                } else {
                    from_hex(&look.chrome.subtext)
                })
                .child(desk::tab_title(desk, tab)),
        )
        .children(zoomed.map(|mark| {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(muted)
                .child(mark)
        }))
        .child(
            div()
                .id(close_selector.clone())
                .debug_selector(move || close_selector.to_string())
                .flex_shrink_0()
                .size(px(18.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(muted)
                .invisible()
                .group_hover("tab", |style| style.visible())
                .when(selected, |close| close.visible())
                .hover(move |style| style.bg(hover))
                .child("✕")
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |desk, _, window, cx| {
                    desk.close_tab(&close_id);
                    desk.after_command(window, cx);
                    cx.stop_propagation();
                })),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |desk, event: &gpui::MouseDownEvent, window, cx| {
                desk.select_tab(&drag_id);
                desk.begin_tab_drag(
                    drag_id.clone(),
                    event.position.x.as_f32(),
                    event.position.y.as_f32(),
                    cx,
                );
                desk.after_command(window, cx);
            }),
        )
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |desk, _, window, cx| {
                desk.close_tab(&middle_id);
                desk.after_command(window, cx);
            }),
        )
        .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
            if event.click_count() >= 2 {
                desk.open_modal(
                    Modal::Rename {
                        tab_id: rename_id.clone(),
                    },
                    window,
                    cx,
                );
            }
        }))
}

fn sidebar_list(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let link = desk.link.as_ref();
    let agent = |id: &ResourceId| link.and_then(|link| link.connection.agents.get(id));
    let panes = link.map_or(&[][..], |link| link.connection.panes());
    let mut agent_panes: Vec<_> = panes
        .iter()
        .filter(|pane| agent(&pane.terminal_id).is_some_and(AgentInfo::needs_you))
        .collect();
    agent_panes.sort_by(|left, right| {
        let (left, right) = (agent(&left.terminal_id), agent(&right.terminal_id));
        agent_rank(left).cmp(&agent_rank(right)).then_with(|| {
            right
                .map(|info| info.changed_at_ms)
                .cmp(&left.map(|info| info.changed_at_ms))
        })
    });
    let sessions = session_groups(desk, look, cx);
    let recent: Vec<_> = desk
        .recent_projects
        .iter()
        .enumerate()
        .map(|(index, path)| recent_folder_row(path.clone(), index, look, cx))
        .collect();
    let session_count = link
        .and_then(|link| link.connection.topology.as_ref())
        .map(|topology| topology.sessions.len())
        .unwrap_or(0);
    div()
        .id("sidebar-list")
        .flex_grow(1.)
        .overflow_y_scroll()
        .px(px(8.))
        .py(px(12.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(organization_bar(desk, look, cx))
        .when(!agent_panes.is_empty(), |list| {
            list.child(section_label("Needs you", look))
                .children(agent_panes.into_iter().map(|pane| {
                    agent_row(desk, pane, agent(&pane.terminal_id), look, cx).into_any_element()
                }))
                .child(div().h(px(8.)))
        })
        .when(!recent.is_empty(), |list| {
            list.child(section_label("Recent folders", look))
                .children(recent)
                .child(div().h(px(8.)))
        })
        .when(session_count == 0, |list| {
            list.child(
                muted(look, "No sessions yet.")
                    .px(px(10.))
                    .py(px(6.))
                    .text_sm(),
            )
        })
        .children(sessions)
}

fn titlebar_lead(sidebar: bool, quick: bool, sidebar_width: f32) -> f32 {
    if quick {
        return MENU_RAIL - 28.;
    }
    if sidebar { sidebar_width } else { MENU_RAIL }
}

/// A stable entry point; the material and interaction live in their own window.
fn main_menu(look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .max_w(px(132.))
        .debug_selector(|| "main-menu-rail".into())
        .occlude()
        .child(
            Button::new("main-menu")
                .label("Phux")
                .icon(IconName::Search)
                .ghost()
                .small()
                .rounded_full()
                .h(px(30.))
                .px(px(10.))
                .w_full()
                .text_color(look.ink)
                .on_click(cx.listener(|desk, _, window, cx| {
                    desk.open_modal(Modal::Goto, window, cx);
                })),
        )
        .into_any_element()
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    #[test]
    fn the_menu_reserves_space_with_or_without_the_sidebar() {
        assert_eq!(titlebar_lead(true, false, 248.), 248.);
        assert_eq!(titlebar_lead(true, false, 180.), 180.);
        assert_eq!(titlebar_lead(false, false, 248.), MENU_RAIL);
        assert!(titlebar_lead(false, true, 248.) > TRAFFIC_LIGHTS);
    }
}

#[cfg(test)]
mod tab_tests;

fn organization_bar(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let enabled = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.organization());
    let count = desk.organization.selected.len();
    let busy = desk.organization.busy();
    if !enabled || (count == 0 && !busy && desk.organization.undo.is_none()) {
        return div().into_any_element();
    }
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .py(px(6.))
        .child(
            muted(
                look,
                if busy {
                    "Moving…".to_owned()
                } else if count > 0 {
                    format!("{count} selected")
                } else {
                    String::new()
                },
            )
            .flex_1(),
        )
        .when(count > 0, |row| {
            row.child(
                Button::new("move-selected-work")
                    .label("Move…")
                    .small()
                    .disabled(busy)
                    .on_click(cx.listener(|desk, _, window, cx| {
                        desk.execute("move-resources", window, cx)
                    })),
            )
            .child(
                ghost_button("clear-work-selection", "Clear")
                    .disabled(busy)
                    .on_click(cx.listener(|desk, _, _, cx| {
                        desk.organization.selected.clear();
                        cx.notify();
                    })),
            )
        })
        .when(desk.organization.undo.is_some(), |row| {
            row.child(
                Button::new("undo-organization")
                    .label("Undo")
                    .small()
                    .disabled(busy)
                    .on_click(cx.listener(|desk, _, window, cx| {
                        desk.execute("undo-organization", window, cx)
                    })),
            )
        })
        .into_any_element()
}

fn recent_folder_row(
    path: PathBuf,
    index: usize,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let hover = look.hover;
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let full_path = path.display().to_string();
    let forgotten = path.clone();
    div()
        .h(px(28.))
        .flex()
        .items_center()
        .child(
            Button::new(SharedString::from(format!("recent-folder-{index}")))
                .label(label)
                .tooltip(full_path)
                .ghost()
                .small()
                .on_click(cx.listener(move |desk, _, window, cx| {
                    desk.open_project_path(path.clone(), window, cx);
                })),
        )
        .child(
            ghost_icon(
                format!("forget-folder-{index}"),
                IconName::Close,
                "Forget folder",
            )
            .on_click(cx.listener(move |desk, _, _, cx| {
                desk.forget_recent_project(&forgotten, cx);
            })),
        )
        .hover(move |style| style.bg(hover))
        .into_any_element()
}

/// One work catalog across authorities. A host is context, never a navigation root.
fn session_groups(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    let mut project = None;
    for work in desk.work_catalog() {
        let group = work
            .project
            .clone()
            .unwrap_or_else(|| (String::new(), "Work".into()));
        if project.as_ref() != Some(&group.0) {
            rows.push(section_label(&group.1, look).into_any_element());
            project = Some(group.0);
        }
        rows.push(if desk.work_is_active(&work.destination) {
            session_block(desk, work.id, &work.destination.name, look, cx)
        } else {
            work_row(work, look, cx)
        });
    }
    for (host, error) in desk.fleet.failures() {
        let name = host.as_deref().unwrap_or("Local");
        rows.push(
            muted(look, format!("{name} · unavailable"))
                .id(SharedString::from(format!("unavailable-{name}")))
                .px(px(10.))
                .py(px(6.))
                .tooltip(move |window, cx| Tooltip::new(error.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    rows
}

/// A sidebar row: icon slot, label, trailing detail; washed on hover.
fn sidebar_row(id: impl Into<gpui::ElementId>, look: &Look) -> gpui::Stateful<gpui::Div> {
    let hover = look.hover;
    div()
        .id(id)
        .min_h(px(SIDEBAR_ROW))
        .px(px(8.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(row_radius(&look.skin)))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
}

/// A row's centred icon slot.
fn row_icon(icon: impl IntoElement) -> gpui::Div {
    div()
        .w(px(SIDEBAR_ICON))
        .flex_shrink_0()
        .flex()
        .justify_center()
        .child(icon)
}

/// A work row's leading glyph.
fn caret(glyph: &'static str, look: &Look) -> gpui::Div {
    div()
        .w(px(SIDEBAR_ICON))
        .text_xs()
        .text_color(from_hex(&look.chrome.faint))
        .child(glyph)
}

/// A work row's name over the chip naming where it runs.
fn work_heading(name: gpui::Div, host: Option<&str>, online: bool, look: &Look) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_start()
        .gap(px(3.))
        .child(name.w_full().truncate().text_sm())
        .child(host_chip(host, online, look))
}

fn work_row(work: super::fleet::Work, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let target = work.destination.clone();
    let tip = format!("{} · Runs on {}", target.name, target.host_label());
    let selector = format!("work-{}-{}", target.host_label(), work.id);
    sidebar_row(SharedString::from(selector.clone()), look)
        .debug_selector(move || selector.clone())
        .min_h(px(52.))
        .child(caret("›", look))
        .child(work_heading(
            div().child(target.name.clone()),
            target.host.as_deref(),
            work.online,
            look,
        ))
        .when(work.needs_you > 0, |row| {
            row.child(accent_pill(&work.needs_you.to_string(), look))
        })
        .opacity(if work.online { 1. } else { 0.55 })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |desk, _, _, cx| desk.navigate_work(target.clone(), cx)))
        .into_any_element()
}

fn intake_badge(designated: bool, awaiting_organization: bool) -> Option<&'static str> {
    if designated {
        Some("intake")
    } else if awaiting_organization {
        Some("imported")
    } else {
        None
    }
}

fn session_block(
    desk: &Desk,
    id: u32,
    name: &str,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(link) = desk.link.as_ref() else {
        return div().into_any_element();
    };
    let panes: Vec<PaneDescriptor> = link
        .connection
        .panes()
        .iter()
        .filter(|pane| pane.session_id == id)
        .cloned()
        .collect();
    let project_path = link
        .connection
        .projects
        .get(&id)
        .and_then(|_| link.connection.directories.get(&id))
        .map(PathBuf::from);
    let collapsed = desk.collapsed.contains(name);
    let session = link
        .connection
        .topology
        .as_ref()
        .and_then(|topology| topology.sessions.iter().find(|session| session.id == id));
    let destination = session.and_then(|session| session.key);
    let designated = link.connection.is_intake_session(id);
    let awaiting = session.is_some_and(|session| session.awaiting_organization);
    let intake = session
        .filter(|_| awaiting && !designated)
        .and_then(|session| session.key);
    let organizing = link.connection.organization();
    let rename_target = desk.rename_session_target(id);
    let handle = cx.weak_entity();
    let session_name = name.to_owned();
    let menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
        let (rename, kill, select) = (handle.clone(), handle.clone(), handle.clone());
        let (rename_name, kill_name) = (session_name.clone(), session_name.clone());
        let rename_target = rename_target.clone();
        menu.when(organizing, |menu| {
            menu.item(PopupMenuItem::new("Select All").on_click(move |_, _, cx| {
                let _ = select.update(cx, |desk, cx| desk.select_session_work(id, cx));
            }))
        })
        .item(
            PopupMenuItem::new("Rename Session…").on_click(move |_, window, cx| {
                let current = rename_name.clone();
                let _ = rename.update(cx, |desk, cx| {
                    desk.open_modal(
                        Modal::Session {
                            create: false,
                            current,
                            target: rename_target.clone(),
                        },
                        window,
                        cx,
                    )
                });
            }),
        )
        .when_some(intake, |menu, key| {
            let organized = handle.clone();
            let intake_name = session_name.clone();
            menu.item(
                PopupMenuItem::new("Mark organized").on_click(move |_, _, cx| {
                    let name = intake_name.clone();
                    let _ = organized.update(cx, |desk, cx| desk.resolve_intake(key, name, cx));
                }),
            )
        })
        .when(!designated, |menu| {
            menu.item(
                PopupMenuItem::new("Kill Session…").on_click(move |_, window, cx| {
                    let name = kill_name.clone();
                    let _ = kill.update(cx, |desk, cx| {
                        desk.open_modal(Modal::KillSession { name, session: id }, window, cx)
                    });
                }),
            )
        })
    };
    let toggle_name = name.to_owned();
    let extra: Vec<ResourceRecord> = destination
        .and_then(|key| {
            desk.client().map(|client| {
                client
                    .organization()
                    .resources()
                    .iter()
                    .filter(|resource| {
                        resource.session == key
                            && resource.kind != ResourceKind::AgentSession
                            && running(resource)
                            && !panes.iter().any(|pane| pane.key == Some(resource.key))
                    })
                    .cloned()
                    .collect()
            })
        })
        .unwrap_or_default();
    let badge = intake_badge(designated, awaiting);
    let details = match badge {
        Some("intake") => "Designated home for unorganized work",
        Some(_) => "Imported session awaiting organization",
        None => "Session",
    };
    let session_tip = format!(
        "{name} · Runs on {} · {details} · {} terminals",
        desk.remote.as_deref().unwrap_or("Local"),
        panes.len() + extra.len()
    );
    let header = sidebar_row(SharedString::from(format!("session-{id}")), look)
        .group("session")
        .min_h(px(52.))
        .py_0()
        .bg(host_tint(desk.remote.as_deref(), look).opacity(0.07))
        .child(caret(if collapsed { "▸" } else { "▾" }, look))
        .child(work_heading(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(name.to_owned()),
            desk.remote.as_deref(),
            link.connection.status == Status::Attached,
            look,
        ))
        .when(badge == Some("imported"), |row| {
            row.child(accent_pill("imported", look))
        })
        .when_some(project_path, |header, path| {
            header.child(
                ghost_icon(
                    format!("project-open-{id}"),
                    IconName::Folder,
                    format!("Open project window: {}", path.display()),
                )
                .on_click(cx.listener(move |desk, _, window, cx| {
                    cx.stop_propagation();
                    desk.open_project_path(path.clone(), window, cx);
                })),
            )
        })
        .child(
            div()
                .invisible()
                .group_hover("session", |style| style.visible())
                .child(
                    ghost_icon(
                        format!("session-new-{id}"),
                        IconName::Plus,
                        "New terminal in this session",
                    )
                    .on_click(cx.listener(move |desk, _, window, cx| {
                        cx.stop_propagation();
                        desk.new_terminal_in(id, window, cx);
                    })),
                ),
        )
        .child(
            ghost_icon(
                format!("session-menu-{id}"),
                IconName::Ellipsis,
                "Session actions",
            )
            .dropdown_menu(menu.clone()),
        )
        .on_click(
            cx.listener(move |desk, _, _, cx| desk.toggle_session_collapsed(&toggle_name, cx)),
        )
        .when_some(destination, |row, key| {
            let destination_name = name.to_owned();
            let wash = from_hex(&look.chrome.accent_wash);
            row.drag_over::<ResourceDrag>(move |style, _, _, _| style.bg(wash))
                .on_drop(cx.listener(move |desk, drag: &ResourceDrag, window, cx| {
                    desk.move_resources_to(
                        drag.keys.clone(),
                        key,
                        destination_name.clone(),
                        window,
                        cx,
                    );
                }))
        })
        .tooltip(move |window, cx| Tooltip::new(session_tip.clone()).build(window, cx))
        .context_menu(menu);
    div()
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(header)
        .when(!collapsed, |block| {
            block
                .children(
                    panes
                        .iter()
                        .map(|pane| pane_row(desk, pane, look, cx).into_any_element()),
                )
                .children(
                    extra
                        .iter()
                        .map(|resource| record_row(desk, resource, look, cx)),
                )
        })
        .into_any_element()
}

/// A record worth a sidebar row: one whose process is coming up, up, or going
/// down. An exited, failed, lost, or never-started record has nothing to show
/// or attach to, so it stays out of the list.
fn running(resource: &ResourceRecord) -> bool {
    matches!(
        exec_state(resource),
        Some(ExecState::Starting | ExecState::Running | ExecState::Stopping)
    )
}

fn exec_state(resource: &ResourceRecord) -> Option<ExecState> {
    resource
        .activation
        .as_ref()
        .map(|activation| activation.state)
}

fn record_row(
    desk: &Desk,
    resource: &ResourceRecord,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let muted = from_hex(&look.chrome.muted);
    let key = resource.key;
    let selected = desk.organization.selected.contains(&key);
    let kind = match resource.kind {
        ResourceKind::Terminal => "Terminal",
        _ => "Item",
    };
    let label = resource.name.clone().unwrap_or_else(|| kind.to_owned());
    let state = match exec_state(resource) {
        Some(ExecState::Starting) => "Starting",
        Some(ExecState::Running) => "Running",
        Some(ExecState::Stopping) => "Stopping",
        Some(ExecState::Completed) => "Exited",
        Some(ExecState::Failed) => "Failed to start",
        Some(ExecState::Lost | ExecState::Abandoned) => "Lost",
        None => "Not running",
    };
    let tooltip_label = label.clone();
    let keys = desk.organization.drag_keys(key);
    let drag = ResourceDrag {
        label: drag_label(&keys, &label),
        keys,
    };
    sidebar_row(SharedString::from(format!("resource-{key}")), look)
        .when(selected, |row| row.bg(look.active))
        .child(row_icon(dot(6., muted)))
        .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
        .when(state != "Running", |row| {
            row.child(self::muted(look, state))
        })
        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_click(cx.listener(move |desk, _, _, cx| desk.toggle_resource_selection(key, cx)))
        .tooltip(move |window, cx| {
            Tooltip::new(format!("{tooltip_label}\n{kind} · {state}\n{key}")).build(window, cx)
        })
        .into_any_element()
}

fn pane_row(
    desk: &Desk,
    pane: &PaneDescriptor,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let colors = &look.chrome;
    let terminal = pane.terminal_id.clone();
    let key = pane.key;
    let selected = key.is_some_and(|key| desk.organization.selected.contains(&key));
    let title = desk.pane_title(Some(&pane.terminal_id));
    let drag = key.map(|key| {
        let keys = desk.organization.drag_keys(key);
        let label = drag_label(&keys, &title);
        ResourceDrag { keys, label }
    });
    let focused = desk.focused_terminal().as_ref() == Some(&terminal);
    let visible = desk.active_tab().is_some_and(|tab| {
        layout::placements(&tab.root)
            .iter()
            .any(|placement| placement.terminal_id.as_ref() == Some(&terminal))
    });
    let agent = desk.agent(&terminal);
    let glyph = match agent {
        Some(info) => dot(
            8.,
            from_hex(agent_swatch(info.state)).opacity(if info.needs_you() { 1. } else { 0.65 }),
        ),
        None => div()
            .text_xs()
            .text_color(from_hex(if visible {
                &colors.theme.accent
            } else {
                &colors.faint
            }))
            .child("❯"),
    };
    let tooltip_terminal = terminal.clone();
    let handle = cx.weak_entity();
    sidebar_row(SharedString::from(format!("pane-{terminal}")), look)
        .when(focused || selected, |row| row.bg(look.active))
        .child(row_icon(glyph))
        .child(
            div().flex_1().min_w_0().child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(if focused {
                        gpui::FontWeight::SEMIBOLD
                    } else {
                        gpui::FontWeight::MEDIUM
                    })
                    .text_color(if focused {
                        look.ink
                    } else {
                        from_hex(&colors.subtext)
                    })
                    .child(title),
            ),
        )
        .when_some(drag, |row, drag| {
            row.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        })
        .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
            if event.modifiers().platform
                && let Some(key) = key
            {
                desk.toggle_resource_selection(key, cx);
            } else {
                desk.reveal_terminal(&terminal);
            }
            desk.after_command(window, cx);
        }))
        .tooltip(move |window, cx| pane_tooltip(&tooltip_terminal, &handle, window, cx))
}

fn agent_row(
    desk: &Desk,
    pane: &PaneDescriptor,
    agent: Option<&AgentInfo>,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let terminal = pane.terminal_id.clone();
    let focused = desk.focused_terminal().as_ref() == Some(&terminal);
    let urgent = agent.is_some_and(AgentInfo::urgent);
    let state = if urgent && agent.is_none_or(|info| info.state != AgentState::Blocked) {
        "attention"
    } else {
        agent.map_or("", AgentInfo::word)
    };
    let tint = agent_swatch(agent.map_or(AgentState::Unknown, |info| info.state));
    let tooltip_terminal = terminal.clone();
    let handle = cx.weak_entity();
    sidebar_row(SharedString::from(format!("agent-{terminal}")), look)
        .border_1()
        .border_color(if urgent {
            from_hex(&format!("{tint}66"))
        } else {
            gpui::transparent_black()
        })
        .when(focused, |row| row.bg(look.active))
        .when(urgent && !focused, |row| {
            row.bg(from_hex(&format!("{tint}14")))
        })
        .child(row_icon(dot(8., from_hex(tint))))
        .child(
            div().flex_1().min_w_0().child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(desk.pane_title(Some(&pane.terminal_id))),
            ),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(from_hex(tint))
                .child(state),
        )
        .on_click(cx.listener(move |desk, _, window, cx| {
            desk.reveal_terminal(&terminal);
            desk.after_command(window, cx);
        }))
        .tooltip(move |window, cx| pane_tooltip(&tooltip_terminal, &handle, window, cx))
}

/// Detail stays available on hover without repeating it in every sidebar row.
fn pane_tooltip(
    terminal: &ResourceId,
    handle: &WeakEntity<Desk>,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let details = if let Some(desk) = handle.upgrade() {
        let desk = desk.read(cx);
        let mut details = desk.pane_title(Some(terminal));
        if let Some(agent) = desk.agent(terminal) {
            details.push_str(&format!("\n{} · {}", agent.name, agent.word()));
            if let Some(provider) = &agent.kind {
                details.push_str(&format!(" · {provider}"));
            }
            if agent.urgent() {
                details.push_str(" · attention");
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_millis() as u64)
                .unwrap_or(0);
            details.push_str(&format!(" · {}", ago(agent.changed_at_ms, now)));
        }
        if let Some(pane) = desk
            .link
            .as_ref()
            .and_then(|link| link.connection.pane(terminal))
            && let Some(cwd) = &pane.cwd
        {
            details.push_str(&format!("\n{cwd}"));
        }
        details.push_str(&format!("\n{terminal}"));
        details
    } else {
        terminal.to_string()
    };
    Tooltip::new(details).build(window, cx)
}

/// Seconds, minutes, hours or days since `then`, both in Unix milliseconds.
pub(super) fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then) / 1000;
    match seconds {
        0..5 => "now".into(),
        5..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", (seconds + 30) / 60),
        3600..86400 => format!("{}h", (seconds + 1800) / 3600),
        _ => format!("{}d", (seconds + 43200) / 86400),
    }
}

/// Milliseconds from `now` until [`ago`]'s label for `then` next changes.
pub(super) fn ago_wait(then: u64, now: u64) -> u64 {
    let elapsed = now.saturating_sub(then);
    let seconds = elapsed / 1000;
    // Each bucket rounds to its nearest unit, so its label turns at the half unit.
    let turn = |unit: u64| ((seconds + unit / 2) / unit + 1) * unit - unit / 2;
    let next = match seconds {
        0..5 => 5,
        5..60 => seconds + 1,
        60..3600 => turn(60).min(3600),
        3600..86400 => turn(3600).min(86400),
        _ => turn(86400),
    };
    next * 1000 - elapsed
}

fn sidebar_resize(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let active = desk.dragging_sidebar();
    let accent = from_hex(&look.chrome.theme.accent);
    div()
        .id("sidebar-resize")
        .absolute()
        .right(px(-2.))
        .top(px(0.))
        .w(px(5.))
        .h_full()
        .cursor_col_resize()
        .when(active, |handle| handle.bg(accent))
        .hover(move |style| style.bg(accent.opacity(0.5)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|desk, _, _, cx| desk.begin_sidebar_drag(cx)),
        )
}

fn main_column(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let status = look.skin.chrome.status;
    div()
        .flex_grow(1.)
        .min_w(px(0.))
        .flex()
        .flex_col()
        .child(workspace_body(desk, look, window, cx))
        .when(status > 0., |column| {
            column.child(status_bar(desk, status, look, cx))
        })
}

fn workspace_body(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let gutter = look.skin.shape.gutter;
    let zoomed = desk.active_tab().and_then(|tab| tab.zoomed_id.clone());
    let root = desk.active_tab().map(|tab| tab.root.clone());
    div()
        .flex_grow(1.)
        .min_h(px(0.))
        .relative()
        .pt(px(gutter))
        .px(px(gutter))
        .child(if let Some(id) = zoomed {
            desk.placement(&id).cloned().map_or_else(
                || div().into_any_element(),
                |placement| pane_view(desk, &placement, window, cx),
            )
        } else if let Some(root) = root {
            split_tree(desk, &root, look, window, cx).into_any_element()
        } else {
            empty_state(desk, look, cx).into_any_element()
        })
        .children(peek_card(desk, look, cx))
}

fn empty_state(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.link.as_ref().map(|link| link.connection.status);
    let error = desk.connect_error.clone().or_else(|| {
        desk.link
            .as_ref()
            .and_then(|link| link.connection.error.clone())
    });
    let pending = desk
        .link
        .as_ref()
        .is_some_and(|link| link.workspace.pending() > 0);
    let new = if pending {
        "Starting terminal…"
    } else {
        "New terminal"
    };
    let body = match status {
        Some(Status::Attached) => div().flex().flex_col().items_center().gap(px(8.)).children(
            [
                ("empty-new", new, "new", true),
                ("empty-folder", "Open folder…", "folder", false),
                ("empty-palette", "Command palette", "palette", false),
            ]
            .map(|(id, label, command, primary)| {
                empty_action(desk, id, label, command, primary, cx)
            }),
        ),
        Some(Status::Idle | Status::Connecting | Status::Negotiated) => muted(
            look,
            match desk.remote.as_deref() {
                Some(host) => format!("Connecting to {host}…"),
                None => format!("Connecting to {}…", desk.socket.display()),
            },
        )
        .text_sm(),
        _ => div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .max_w(px(520.))
                    .text_center()
                    .text_sm()
                    .text_color(from_hex(&look.chrome.theme.danger))
                    .child(error.unwrap_or_else(|| "The connection closed.".into())),
            )
            .child(empty_action(
                desk,
                "empty-reconnect",
                "Reconnect",
                "reconnect",
                true,
                cx,
            )),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(18.))
        .child(
            div()
                .text_size(px(34.))
                .font_weight(gpui::FontWeight::EXTRA_BOLD)
                .child("phux"),
        )
        .child(body)
}

fn empty_action(
    desk: &Desk,
    id: &'static str,
    label: &str,
    command: &'static str,
    primary: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    let kbd = command_kbd(desk, command);
    let button = Button::new(id)
        .label(label.to_owned())
        .w(px(240.))
        .on_click(cx.listener(move |desk, _, window, cx| desk.execute(command, window, cx)));
    let button = if primary { button.primary() } else { button };
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(button)
        .child(div().w(px(64.)).children(kbd))
}

fn command_kbd(desk: &Desk, id: &str) -> Option<Kbd> {
    keystroke(desk.bound_chord(id)?).map(Kbd::new)
}

/// How present a split child is: a leaf follows its motion, a subtree is settled.
fn node_presence(desk: &Desk, node: &LayoutNode, now: Instant) -> f32 {
    match node {
        LayoutNode::Leaf { placement } => desk.motion.presence(&placement.id, now),
        LayoutNode::Split { .. } => 1.,
    }
}

fn split_tree(
    desk: &mut Desk,
    node: &LayoutNode,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    match node {
        LayoutNode::Leaf { placement } => pane_view(desk, placement, window, cx),
        LayoutNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let horizontal = matches!(axis, Axis::Row);
            let bounds = desk.split_bounds.clone();
            let key = id.clone();
            let now = Instant::now();
            let (lead, trail) = (
                node_presence(desk, first, now),
                node_presence(desk, second, now),
            );
            let painted = motion::ratio(*ratio, lead, trail);
            let shape = &look.skin.shape;
            let gap = shape.gap * lead.min(trail);
            let thick = gap.max(shape.divider);
            // Too thin to grab: the second side lends a wider strip over the line.
            let lend = thick < SPLIT_GRAB;
            let reach = -(SPLIT_GRAB + thick) / 2.;
            div()
                .size_full()
                .flex()
                .when(horizontal, |row| row.flex_row())
                .when(!horizontal, |column| column.flex_col())
                .child(
                    div()
                        .when(horizontal, |side| side.w(relative(painted)).h_full())
                        .when(!horizontal, |side| side.h(relative(painted)).w_full())
                        .flex_shrink_0()
                        .child(split_tree(desk, first, look, window, cx)),
                )
                .child(split_divider(desk, id.clone(), *axis, gap, thick, look, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .when(lend, |side| side.relative())
                        .child(split_tree(desk, second, look, window, cx))
                        .when(lend, |side| {
                            let strip = div().absolute();
                            let strip = if horizontal {
                                strip.top_0().bottom_0().left(px(reach)).w(px(SPLIT_GRAB))
                            } else {
                                strip.left_0().right_0().top(px(reach)).h(px(SPLIT_GRAB))
                            };
                            side.child(split_grab(strip, id.clone(), *axis, cx))
                        }),
                )
                .on_children_prepainted(move |children, _, _| {
                    if let (Some(first), Some(last)) = (children.first(), children.last()) {
                        bounds.borrow_mut().insert(key.clone(), first.union(last));
                    }
                })
                .into_any_element()
        }
    }
}

/// The sheet between two panes, `thick` points across. A skin with a gap
/// grabs it whole and shows a short pill on hover, in the accent while
/// dragged; a skin with a painted divider draws the line, and the second
/// side lends it a wider grab strip.
fn split_divider(
    desk: &Desk,
    id: String,
    axis: Axis,
    gap: f32,
    thick: f32,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let horizontal = matches!(axis, Axis::Row);
    let active = desk.dragging_split() == Some(id.as_str());
    let accent = from_hex(&look.chrome.theme.accent);
    let grip = look.ink.opacity(0.28);
    let divider = look.skin.shape.divider;
    let bar = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .when(horizontal, |bar| bar.w(px(thick)).h_full())
        .when(!horizontal, |bar| bar.h(px(thick)).w_full())
        .when(divider > 0., |bar| {
            // The line takes over the bar as the gap closes onto it.
            bar.bg(if active {
                accent
            } else {
                look.divider.opacity(divider.min(1.) * divider / thick)
            })
        });
    if thick < SPLIT_GRAB {
        return bar.into_any_element();
    }
    split_grab(bar.group("divider"), id, axis, cx)
        .when(gap > divider, |bar| {
            bar.child(
                div()
                    .rounded_full()
                    .when(active, |pill| pill.bg(accent))
                    .when(!active, |pill| {
                        pill.group_hover("divider", move |style| style.bg(grip))
                    })
                    .when(horizontal, |pill| pill.w(px(3.)).h(px(44.)))
                    .when(!horizontal, |pill| pill.h(px(3.)).w(px(44.))),
            )
        })
        .into_any_element()
}

/// Make `area` start a drag of split `id`.
fn split_grab(
    area: gpui::Div,
    id: String,
    axis: Axis,
    cx: &mut Context<Desk>,
) -> gpui::Stateful<gpui::Div> {
    let horizontal = matches!(axis, Axis::Row);
    area.id(SharedString::from(format!("split-{id}")))
        .when(horizontal, |area| area.cursor_col_resize())
        .when(!horizontal, |area| area.cursor_row_resize())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |desk, _, _, cx| {
                desk.begin_split_drag(id.clone(), axis, cx);
                cx.stop_propagation();
            }),
        )
}

/// One pane: a rounded card on the glass, with no header. Its controls float
/// in when the pointer reaches the top edge, and a file dragged over it shows
/// where the drop will paste.
fn pane_view(
    desk: &mut Desk,
    placement: &Placement,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let presence = desk.motion.presence(&placement.id, Instant::now());
    let focused =
        desk.focused_placement().map(|item| item.id.as_str()) == Some(placement.id.as_str());
    let look = desk.look();
    let colors = &look.chrome;
    let placement_id = placement.id.clone();
    let terminal = placement.terminal_id.clone();
    let view = placement.view_id;
    // The terminal holds the keyboard only while no modal or find field does.
    let settings = desk.terminal_settings(placement, focused && !desk.text_field_has_keys());
    let client = desk.client();
    desk.prepare_pending_native(&placement_id, window, cx);
    let body = if desk.conversation_visible(&placement_id) {
        super::conversation::render(desk, &placement_id, colors, window, cx)
    } else if let (Some(terminal), Some(view), Some(settings), Some(client)) =
        (terminal, view, settings, client)
    {
        let remote = desk.remote.clone();
        let entity = desk.panes_mut().entity_for(
            &placement_id,
            &terminal,
            view,
            settings,
            &client,
            window,
            cx,
        );
        entity.update(cx, |terminal, cx| {
            terminal.set_window_remote(remote, cx);
        });
        // A cached view keeps its last paint until this entity is notified.
        // Chrome refreshes (hover, clock, sidebar) must not reacquire and
        // reshape the terminal. GPUI requires a definite size for that cache.
        div()
            .size_full()
            .child(entity.cached(gpui::StyleRefinement::default().size_full()))
            .into_any_element()
    } else {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(from_hex(&colors.muted))
            .child(if placement.terminal_id.is_none() {
                "Saved terminal is not on this server"
            } else {
                "Waiting for terminal"
            })
            .into_any_element()
    };
    let tab = desk.active_tab();
    let zoomed = tab.is_some_and(|tab| tab.zoomed_id.is_some());
    // Focus only shows among several visible panes.
    let shared = !zoomed && tab.is_some_and(|tab| layout::placements(&tab.root).len() > 1);
    let find = desk
        .find
        .as_ref()
        .filter(|find| find.placement_id == placement.id)
        .map(|find| find.case_sensitive);
    let find_bar = find.map(|case| overlays::find_bar(desk, case, cx));
    let dim = (!focused && shared)
        .then_some(1. - desk.prefs.unfocused_opacity as f32)
        .filter(|amount| *amount > 0.);
    let fade = from_hex(&colors.theme.background);
    let agent = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.agent(id))
        .cloned();
    let urgent = agent.as_ref().is_some_and(AgentInfo::urgent);
    let fenced = placement.terminal_id.as_ref().is_some_and(|id| {
        desk.link
            .as_ref()
            .is_some_and(|link| link.connection.fenced(id))
    });
    let id = placement.id.clone();
    let dropping = desk.drop_target.as_deref() == Some(id.as_str());
    let remote_files = placement.terminal_id.as_ref().is_some_and(|terminal| {
        !matches!(
            crate::attach::terminal_host(desk.remote.as_deref(), terminal),
            crate::attach::TerminalHost::Local
        )
    });
    let leaving = desk.motion.is_leaving(&id);
    let controls = find.is_none()
        && !dropping
        && !leaving
        && desk.dragging_split().is_none()
        && (desk.chrome_hover.as_deref() == Some(id.as_str())
            || desk.controls_hover.as_deref() == Some(id.as_str())
            || desk.pane_menu_open.as_deref() == Some(id.as_str()));
    let edge = if dropping {
        from_hex(&colors.theme.accent)
    } else if urgent {
        agent
            .as_ref()
            .map(|info| from_hex(agent_swatch(info.state)).opacity(0.75))
            .unwrap_or(look.lit)
    } else if focused && shared {
        look.lit
    } else {
        gpui::transparent_black()
    };
    let shine = look
        .lit
        .opacity(if focused { 0.7 } else { 0.35 } * gloss(&look.skin));
    let controls_bar =
        controls.then(|| pane_controls(desk, placement, zoomed, &look, cx).into_any_element());
    let conversation_toggle =
        desk.conversation_eligible(&id) && !desk.conversation_visible(&id) && !controls;
    let conversation_id = id.clone();
    let drag_id = id.clone();
    let drop_id = id.clone();
    let exit_id = id.clone();
    div()
        .id(SharedString::from(format!("pane-card-{id}")))
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .when(presence < 1., |card| card.opacity(presence.max(0.)))
        .surface(&look, Surface::Pane)
        .border_color(edge)
        .shadow(pane_shadows(&look))
        .overflow_hidden()
        .block_mouse_except_scroll()
        .on_drag_move::<ExternalPaths>(cx.listener(
            move |desk, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                let inside = event.bounds.contains(&event.event.position);
                let here = desk.drop_target.as_deref() == Some(drag_id.as_str());
                if inside && !here {
                    desk.drop_target = Some(drag_id.clone());
                    cx.notify();
                } else if !inside && here {
                    desk.drop_target = None;
                    cx.notify();
                }
            },
        ))
        .on_drop(cx.listener(move |desk, paths: &ExternalPaths, window, cx| {
            if let Some(tab) = desk.active_tab_id() {
                desk.focus_placement(&tab, &drop_id, window, cx);
            }
            desk.drop_paths(&drop_id, paths.paths(), cx);
            desk.after_command(window, cx);
        }))
        .on_file_drop_exit(cx.listener(move |desk, _: &FileDropEvent, _, cx| {
            if desk.drop_target.as_deref() == Some(exit_id.as_str()) {
                desk.drop_target = None;
                cx.notify();
            }
        }))
        .child(
            div()
                .flex_grow(1.)
                .min_h(px(0.))
                .relative()
                .px(px(desk.prefs.padding_x as f32))
                .py(px(desk.prefs.padding_y as f32))
                .child(body)
                .children(
                    dim.map(|amount| div().absolute().inset_0().bg(Hsla { a: amount, ..fade })),
                ),
        )
        .children(specular(shine, look.skin.shape.radius))
        .child(
            // Reveals the controls. It holds no other listener, so presses and
            // drags still reach the terminal under it.
            div()
                .id(SharedString::from(format!("pane-edge-{id}")))
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(CONTROLS_EDGE))
                .on_hover(track_hover(id.clone(), |desk| &mut desk.chrome_hover, cx)),
        )
        .when(fenced && !controls, |card| {
            card.child(div().absolute().top(px(8.)).right(px(10.)).child(pill(
                "Typing may not have arrived",
                &format!("{}22", colors.theme.warning),
                &colors.theme.warning,
                &look,
            )))
        })
        .children(controls_bar)
        .when(conversation_toggle, |card| {
            card.child(div().absolute().top(px(8.)).right(px(10.)).child(
                ghost_button(format!("conversation-open-{id}"), "Conversation").on_click(
                    cx.listener(move |desk, _, window, cx| {
                        desk.toggle_conversation(&conversation_id, window, cx);
                    }),
                ),
            ))
        })
        .when(dropping, |card| card.child(drop_hint(&look, remote_files)))
        .children(find_bar)
        .into_any_element()
}

/// The floating capsule a pane shows at its top edge: what runs there, then
/// split, zoom and close.
fn pane_controls(
    desk: &Desk,
    placement: &Placement,
    zoomed: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let title = desk.pane_title(placement.terminal_id.as_ref());
    let agent = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.agent(id))
        .cloned();
    let id = placement.id.clone();
    let focus_id = placement.id.clone();
    let zoom = on_pane(id.clone(), "zoom");
    let zoom_label = if zoomed { "Restore" } else { "Zoom" };
    div()
        .id(SharedString::from(format!("pane-controls-{id}")))
        .absolute()
        .top(px(7.))
        .right(px(8.))
        .max_w(relative(0.9))
        .h(px(28.))
        .pl(px(10.))
        .pr(px(3.))
        .flex()
        .items_center()
        .gap(px(6.))
        .surface(look, Surface::Capsule)
        .occlude()
        .text_xs()
        .on_hover(track_hover(id.clone(), |desk| &mut desk.controls_hover, cx))
        .children(
            agent
                .as_ref()
                .map(|info| dot(7., from_hex(agent_swatch(info.state)))),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(look.ink)
                .child(match agent.as_ref().map(|info| (info, info.word())) {
                    Some((info, "")) => info.name.clone(),
                    Some((info, word)) => format!("{} · {word}", info.name),
                    None => title,
                }),
        )
        .child(div().w(px(2.)))
        .when(desk.conversation_eligible(&id), |row| {
            let conversation_id = id.clone();
            row.child(
                ghost_button(
                    format!("pane-conversation-{id}"),
                    if desk.conversation_visible(&id) {
                        "Terminal"
                    } else {
                        "Conversation"
                    },
                )
                .on_click(cx.listener(move |desk, _, window, cx| {
                    cx.stop_propagation();
                    desk.toggle_conversation(&conversation_id, window, cx);
                })),
            )
        })
        .child(
            ghost_icon(
                format!("zoom-{id}"),
                if zoomed {
                    IconName::Minimize
                } else {
                    IconName::Maximize
                },
                zoom_label,
            )
            .on_click(cx.listener(move |desk, _, window, cx| {
                cx.stop_propagation();
                zoom(desk, window, cx);
            })),
        )
        .child({
            let handle = cx.weak_entity();
            let placement = id.clone();
            let open_handle = handle.clone();
            let open_placement = placement.clone();
            let selector = id.clone();
            div()
                .debug_selector(move || format!("pane-actions-{selector}"))
                .child(
                    ghost_icon(
                        format!("pane-more-{id}"),
                        IconName::Ellipsis,
                        "Pane actions",
                    )
                    .dropdown_menu(move |mut menu, _, _| {
                        for (command, label) in [
                            ("split-right", "Split right"),
                            ("split-down", "Split down"),
                            ("view", "Mirror pane"),
                            ("close", "Close pane"),
                        ] {
                            let handle = handle.clone();
                            let placement = placement.clone();
                            if command == "close" {
                                menu = menu.separator();
                            }
                            menu = menu.item(PopupMenuItem::new(label).on_click(
                                move |_, window, cx| {
                                    let _ = handle.update(cx, |desk, cx| {
                                        on_pane(placement.clone(), command)(desk, window, cx)
                                    });
                                },
                            ));
                        }
                        menu
                    })
                    .on_open_change(move |open, _, cx| {
                        let _ = open_handle.update(cx, |desk, cx| {
                            desk.pane_menu_open = open.then(|| open_placement.clone());
                            cx.notify();
                        });
                    }),
                )
        })
        .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
            if let Some(tab) = desk.active_tab_id() {
                desk.focus_placement(&tab, &focus_id, window, cx);
            }
            if event.click_count() >= 2 {
                desk.execute("zoom", window, cx);
            }
        }))
}

/// Shown over the pane a file drag from the OS is above.
fn drop_hint(look: &Look, remote: bool) -> impl IntoElement {
    let accent = from_hex(&look.chrome.theme.accent);
    let (title, detail) = if remote {
        (
            "Drop to upload",
            "Sent to that machine, then its path is pasted",
        )
    } else {
        ("Drop to attach", "Paths paste here, ready for your agent")
    };
    div()
        .absolute()
        .inset_0()
        .rounded(px(look.skin.shape.radius))
        .bg(accent.opacity(0.1))
        .border_2()
        .border_color(accent.opacity(0.8))
        .flex()
        .items_center()
        .justify_center()
        .child(
            notice_card(look, 0.92)
                .px(px(16.))
                .py(px(10.))
                .rounded(px(look.skin.shape.card_radius - 2.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(look.ink)
                        .child(title),
                )
                .child(muted(look, detail)),
        )
}

/// Focus this pane, then run `command` on it.
fn on_pane(
    placement: String,
    command: &'static str,
) -> impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static {
    move |desk, window, cx| {
        if let Some(tab) = desk.active_tab_id() {
            desk.focus_placement(&tab, &placement, window, cx);
        }
        desk.execute(command, window, cx);
    }
}

/// Keep the desk field `slot` picks naming pane `id` while the pointer is over it.
fn track_hover(
    id: String,
    slot: fn(&mut Desk) -> &mut Option<String>,
    cx: &mut Context<Desk>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    cx.listener(move |desk, hovered: &bool, _, cx| {
        let slot = slot(desk);
        if *hovered {
            *slot = Some(id.clone());
        } else if slot.as_deref() == Some(id.as_str()) {
            *slot = None;
        }
        cx.notify();
    })
}

/// Exceptions only: an unhealthy connection, agents that need a person, a
/// terminal still starting, and a pane scrolled away from live output.
fn status_bar(desk: &Desk, height: f32, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let colors = &look.chrome;
    let radius = chip_radius(&look.skin);
    let chip = |id: &'static str| div().id(id).rounded(px(radius));
    let (label, tone) = desk.connection_label();
    let unhealthy = desk.unhealthy();
    let scrolled = desk.session_is_attached()
        && desk
            .focused_view()
            .and_then(|view| desk.client().and_then(|client| client.acquire_view(view)))
            .is_some_and(|frame| !frame.scrollbar.at_tail());
    let (pending, waiting) = desk.link.as_ref().map_or((0, 0), |link| {
        let agents = link.connection.agents.values();
        (
            link.workspace.pending(),
            agents.filter(|info| info.needs_you()).count(),
        )
    });
    let tone_color = from_hex(match tone {
        Tone::Good => &colors.theme.success,
        Tone::Busy => &colors.theme.warning,
        Tone::Bad => &colors.theme.danger,
    });
    let hover = look.hover;
    div()
        .h(px(height))
        .w_full()
        .flex_shrink_0()
        .overflow_hidden()
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .text_xs()
        .text_color(from_hex(&colors.muted))
        .when(tone != Tone::Good, |bar| {
            bar.child(
                chip("status-connection")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(4.))
                    .child(dot(7., tone_color))
                    .child(div().text_color(tone_color).child(label))
                    .when(unhealthy, |status| {
                        status
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .on_click(cx.listener(|desk, _, window, cx| {
                                desk.execute("reconnect", window, cx)
                            }))
                    }),
            )
        })
        .when(waiting > 0, |bar| {
            bar.child(
                chip("status-agents")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(4.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(dot(6., from_hex(agent_swatch(AgentState::Blocked))))
                    .child(format!(
                        "{waiting} {} you",
                        if waiting == 1 { "needs" } else { "need" }
                    ))
                    .on_click(
                        cx.listener(|desk, _, window, cx| desk.execute("attention", window, cx)),
                    ),
            )
        })
        .when(pending > 0, |bar| bar.child("starting terminal…"))
        .child(div().flex_grow(1.))
        .when(scrolled, |bar| {
            bar.child(
                chip("status-scrolled")
                    .px(px(6.))
                    .bg(from_hex(&colors.accent_wash))
                    .text_color(from_hex(&colors.theme.accent))
                    .cursor_pointer()
                    .child("scrolled back · follow live")
                    .on_click(
                        cx.listener(|desk, _, window, cx| desk.execute("follow", window, cx)),
                    ),
            )
        })
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
                |entry| notification_edge(entry, look),
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
            .font_weight(gpui::FontWeight::BOLD)
            .child(count)
    });
    div()
        .relative()
        .child(icon_button(
            "notifications",
            IconName::Bell,
            "Inbox & notifications · badge counts unread alerts",
            desk.shown_chord("notifications"),
            cx.listener(|desk, _, window, cx| desk.execute("notifications", window, cx)),
        ))
        .children(badge)
}

/// The colour a notification wears: its agent's state, or its kind.
pub(super) fn notification_edge(entry: &Notification, look: &Look) -> Hsla {
    let theme = &look.chrome.theme;
    from_hex(match entry.kind {
        Kind::Error => &theme.danger,
        Kind::Agent => match entry.agent_state {
            Some(state) => agent_swatch(state),
            None => &theme.warning,
        },
        Kind::Approval => &theme.warning,
        Kind::Info | Kind::Refusal | Kind::Command => &theme.accent,
    })
}

/// A notification's title over up to `lines` of its body; `strong` sets the
/// title in medium weight.
pub(super) fn notification_text(
    entry: &Notification,
    look: &Look,
    strong: bool,
    lines: usize,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_sm()
                .when(strong, |title| title.font_weight(gpui::FontWeight::MEDIUM))
                .text_color(look.ink)
                .child(entry.title.clone()),
        )
        .children(
            entry
                .body
                .clone()
                .map(|body| muted(look, body).line_clamp(lines)),
        )
}

/// The newest agent or error entry, briefly, over the workspace. Everything
/// else waits behind the bell.
fn peek_card(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> Option<impl IntoElement> {
    let colors = &look.chrome;
    if desk.modal == Modal::Notifications {
        return None;
    }
    let entry = desk.feed.peek()?;
    let id = entry.id;
    let more = desk.feed.unread().count().saturating_sub(1);
    Some(
        notice_card(look, 0.94)
            .id(("notification-peek", id as usize))
            .absolute()
            .bottom(px(12.))
            .right(px(12.))
            .w(px(340.))
            .p(px(10.))
            .pl(px(12.))
            .border_l_4()
            .border_color(notification_edge(entry, look))
            .occlude()
            .cursor_pointer()
            .flex()
            .gap(px(8.))
            .child(
                notification_text(entry, look, true, 3).when(more > 0, |text| {
                    text.child(
                        div()
                            .text_xs()
                            .text_color(from_hex(&colors.subtext))
                            .child(format!("+{more} unread alerts")),
                    )
                }),
            )
            .child(
                ghost_icon(
                    ("notification-peek-dismiss", id as usize),
                    IconName::Close,
                    "Dismiss",
                )
                .on_click(cx.listener(move |desk, _, _, cx| {
                    cx.stop_propagation();
                    desk.feed.read(id);
                    cx.notify();
                })),
            )
            .on_click(cx.listener(move |desk, _, window, cx| {
                desk.open_notification(id, window, cx);
            })),
    )
}

fn section_label(title: &str, look: &Look) -> gpui::Div {
    muted(look, div().flex_1().child(title.to_owned()))
        .px(px(8.))
        .pt(px(8.))
        .h(px(28.))
        .flex()
        .items_center()
        .font_weight(gpui::FontWeight::MEDIUM)
}

/// Secondary text: extra-small, in the theme's muted ink.
pub(super) fn muted(look: &Look, text: impl IntoElement) -> gpui::Div {
    div()
        .text_xs()
        .text_color(from_hex(&look.chrome.muted))
        .child(text)
}

/// A small label chip, `color` text on `background`.
fn pill(text: &str, background: &str, color: &str, look: &Look) -> gpui::Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .rounded(px(look.skin.shape.kit_radius))
        .bg(from_hex(background))
        .text_xs()
        .text_color(from_hex(color))
        .child(text.to_owned())
}

fn accent_pill(text: &str, look: &Look) -> gpui::Div {
    pill(
        text,
        &look.chrome.accent_wash,
        &look.chrome.theme.accent,
        look,
    )
}

/// A raised notice over the panes: the card surface on a near-opaque
/// backing, with the kit's large lift when the skin casts shadows.
pub(super) fn notice_card(look: &Look, opacity: f32) -> gpui::Div {
    div()
        .surface(look, Surface::Card)
        .bg(from_hex(&look.chrome.raised).opacity(opacity))
        .when(!look.popover_shadow.is_empty(), |card| card.shadow_lg())
}

/// A small ghost icon button whose `label` names it and is its tooltip.
pub(super) fn ghost_icon(
    id: impl Into<gpui::ElementId>,
    icon: impl Into<gpui_kit::component::Icon>,
    label: impl Into<SharedString>,
) -> Button {
    let label = label.into();
    Button::new(id)
        .icon(icon.into())
        .ghost()
        .xsmall()
        .accessibility_label(label.clone())
        .tooltip(label)
}

/// A small ghost button showing `label`.
pub(super) fn ghost_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> Button {
    Button::new(id).label(label).ghost().small()
}

/// A chrome icon button that keeps the titlebar from dragging the window when pressed.
fn icon_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    chord: Option<String>,
    click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let button = ghost_icon(id, icon, label).small().on_click(click);
    div().occlude().child(match chord {
        Some(chord) => button.tooltip(format!("{label}  {chord}")),
        None => button,
    })
}

/// `chord` as a GPUI keystroke, when it parses.
pub(super) fn keystroke(chord: &str) -> Option<Keystroke> {
    Keystroke::parse(&gpui_stroke(chord)?).ok()
}

/// What a sidebar drag carries: the row's own name, or how many terminals.
fn drag_label(keys: &[ResourceKey], name: &str) -> String {
    match keys.len() {
        0 | 1 => name.to_owned(),
        count => format!("{count} terminals"),
    }
}

fn agent_rank(agent: Option<&AgentInfo>) -> u8 {
    state_rank(agent.map_or(AgentState::Unknown, |info| info.state))
}

/// The most pressing agent state in `tab`, only when it needs a person.
fn strongest_agent(desk: &Desk, tab: &DeskTab) -> Option<AgentState> {
    layout::placements(&tab.root)
        .into_iter()
        .filter_map(|placement| placement.terminal_id.as_ref().and_then(|id| desk.agent(id)))
        .filter(|info| info.needs_you())
        .map(|info| info.state)
        .min_by_key(|state| state_rank(*state))
}

/// Agent lifecycle colours stay fixed across themes.
fn agent_swatch(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "#f9e2af",
        AgentState::Blocked => "#f38ba8",
        AgentState::Done => "#94e2d5",
        AgentState::Idle => "#a6e3a1",
        AgentState::Unknown => "#7f849c",
    }
}

#[cfg(test)]
mod intake_tests {
    use super::intake_badge;

    #[test]
    fn badges_distinguish_designated_intake_from_imported_sessions() {
        assert_eq!(intake_badge(true, false), Some("intake"));
        assert_eq!(intake_badge(true, true), Some("intake"));
        assert_eq!(intake_badge(false, true), Some("imported"));
        assert_eq!(intake_badge(false, false), None);
        for badge in [intake_badge(true, false), intake_badge(false, true)] {
            assert!(badge.unwrap().len() <= 8);
        }
    }
}

#[cfg(test)]
mod age_tests {
    use super::{ago, ago_wait};

    #[test]
    fn the_wait_ends_exactly_when_the_label_changes() {
        let mut now = 0;
        while now < 4 * 86_400_000 {
            let wait = ago_wait(0, now);
            assert!(wait > 0);
            assert_eq!(ago(0, now), ago(0, now + wait - 1), "at {now}");
            assert_ne!(ago(0, now), ago(0, now + wait), "at {now}");
            now += wait;
        }
    }

    #[test]
    fn minutes_and_hours_sleep_past_seconds() {
        assert_eq!(ago_wait(0, 3_000), 2_000);
        assert_eq!(ago_wait(0, 10_500), 500);
        assert_eq!(ago_wait(0, 120_000), 30_000);
        assert_eq!(ago_wait(0, 2 * 3_600_000), 1_800_000);
    }
}
