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
#[cfg(test)]
use phux_protocol::ids::SessionKey;
use phux_protocol::wire::frame::{ExecState, ResourceRecord};
use phux_protocol::{ResourceId, ResourceKind};

use crate::connection::{AgentInfo, AgentState, state_rank};
use crate::settings::theme::Palette;
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

/// The selected tab's lift, plus a specular top edge and a faint accent glow
/// while the skin is glassy.
fn tab_glow(look: &Look) -> Vec<BoxShadow> {
    let mut shadows = look.tab_shadow.clone();
    let gloss = gloss(&look.skin);
    if gloss > 0. && look.lit.a > 0. {
        shadows.push(BoxShadow {
            color: look.lit.opacity(0.5 * gloss),
            offset: point(px(0.), px(1.)),
            blur_radius: px(0.),
            spread_radius: px(0.),
            inset: true,
        });
        shadows.push(BoxShadow {
            color: from_hex(&look.colors.theme.accent).opacity(0.1 * gloss),
            offset: point(px(0.), px(0.)),
            blur_radius: px(12.),
            spread_radius: px(-4.),
            inset: false,
        });
    }
    shadows
}

/// A pane card's shadow plus a faint inner rim while the skin is glassy.
fn pane_shadows(look: &Look) -> Vec<BoxShadow> {
    let mut shadows = look.pane_shadow.clone();
    let rim = look.rim.opacity(0.6 * gloss(&look.skin));
    if rim.a > 0. {
        shadows.push(BoxShadow {
            color: rim,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: true,
        });
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
fn host_tint(host: Option<&str>, colors: &Palette) -> Hsla {
    let hue = host.map_or(190., |name| {
        let hash = name.bytes().fold(2_166_136_261_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
        });
        (hash % 360) as f32
    });
    gpui::hsla(
        hue / 360.,
        0.58,
        if colors.theme.appearance == "light" {
            0.38
        } else {
            0.72
        },
        1.,
    )
}

fn host_chip(host: Option<&str>, online: bool, colors: &Palette) -> impl IntoElement {
    let label = host.unwrap_or("Local");
    let tint = host_tint(host, colors);
    let detail = format!("Runs on {label}");
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
        .child(div().size(px(5.)).flex_shrink_0().rounded_full().bg(tint))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(10.))
                .text_color(from_hex(&colors.subtext))
                .child(detail),
        )
        .when(!online, |chip| {
            chip.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(10.))
                    .text_color(from_hex(&colors.muted))
                    .child("· offline"),
            )
        })
}

fn workspace_identity(desk: &Desk, colors: &Palette) -> impl IntoElement {
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
            colors,
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

fn cached_chrome(
    build: fn(&mut Desk, &mut Window, &mut Context<Desk>) -> AnyElement,
    cx: &mut Context<Desk>,
) -> Entity<CachedChrome> {
    let observed = cx.entity();
    cx.new(|cx| {
        cx.observe(&observed, |_, _, cx| cx.notify()).detach();
        CachedChrome {
            desk: observed.downgrade(),
            build,
        }
    })
}

fn chrome_slot(
    slot: &mut Option<Entity<CachedChrome>>,
    build: fn(&mut Desk, &mut Window, &mut Context<Desk>) -> AnyElement,
    style: StyleRefinement,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    slot.get_or_insert_with(|| cached_chrome(build, cx))
        .clone()
        .cached(style)
}

fn build_titlebar(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let sidebar = desk.prefs.sidebar_visible && desk.role != desk::WindowRole::Quick;
    let look = desk.look();
    titlebar(desk, sidebar, &look, cx).into_any_element()
}

fn build_sidebar(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    sidebar_column(desk, &look.colors, cx).into_any_element()
}

fn build_tab_rail(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    let colors = &look.colors;
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
            div()
                .px(px(10.))
                .pb(px(10.))
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(from_hex(&colors.muted))
                .child("Workspace"),
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
    let look = desk.look();
    overlays::layer(desk, &look.colors, window, cx)
}

pub fn render(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    let _frame = crate::diag::FRAME.timer();
    let sidebar = desk.prefs.sidebar_visible && desk.role != desk::WindowRole::Quick;
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
    let sidebar_width = desk.prefs.sidebar_width as f32 * presence;
    let tab_count = desk
        .link
        .as_ref()
        .map_or(0, |link| link.workspace.tabs.len());
    let side_tabs = desk.tab_position() == TabPosition::Side && tabs_shown(&skin, tab_count);
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
                .when(sidebar && presence > 0., |row| {
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
fn titlebar(
    desk: &mut Desk,
    sidebar: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let region = desk
        .drag_region
        .get_or_insert_with(|| cx.new(crate::chrome::DragRegion::new))
        .clone();
    let colors = &look.colors;
    let skin = &look.skin;
    let height = skin.chrome.titlebar;
    let fit = controls_fit(height);
    let quick = desk.role == desk::WindowRole::Quick;
    let presence = skin.chrome.sidebar.clamp(0., 1.);
    let full = titlebar_lead(
        sidebar && presence > 0.,
        quick,
        desk.prefs.sidebar_width as f32 * presence,
    );
    let lead = (TRAFFIC_LIGHTS + (full - TRAFFIC_LIGHTS) * fit).max(TRAFFIC_LIGHTS);
    let tabs = desk
        .link
        .as_ref()
        .map_or(0, |link| link.workspace.tabs.len());
    let top_tabs = desk.tab_position() == TabPosition::Top && tabs_shown(skin, tabs);
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
                        .text_color(from_hex(&colors.subtext))
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
                    lead.child(main_menu(desk, cx)).when(!quick, |lead| {
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
                    .child(workspace_identity(desk, colors)),
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
                            .child(
                                div().occlude().child(
                                    Button::new("launch-native-agent")
                                        .icon(IconName::Bot)
                                        .accessibility_label("New agent")
                                        .ghost()
                                        .small()
                                        .tooltip("New agent")
                                        .on_click(cx.listener(|desk, _, window, cx| {
                                            desk.execute("native-agent", window, cx)
                                        })),
                                ),
                            )
                            .child(icon_button(
                                "new-tab",
                                IconName::Plus,
                                "New Terminal",
                                desk.shown_chord("new"),
                                cx.listener(|desk, _, window, cx| desk.execute("new", window, cx)),
                            ))
                            .child(bell(desk, colors, cx)),
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
                            skin_toggle_label(desk.prefs.plain),
                            desk.shown_chord("plain-terminal"),
                            cx.listener(|desk, _, window, cx| {
                                desk.execute("plain-terminal", window, cx)
                            }),
                        )),
                )
                .child(update_button(desk, cx)),
        )
}

/// What the skin toggle offers: the skin it switches to.
fn skin_toggle_label(plain: bool) -> &'static str {
    if plain {
        "Liquid Glass"
    } else {
        "Plain terminal"
    }
}

/// One tab renderer and one set of tab actions for both placements.
fn tab_strip(
    desk: &Desk,
    position: TabPosition,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let tabs = desk
        .link
        .as_ref()
        .map(|link| link.workspace.tabs.as_slice())
        .unwrap_or_default();
    let active = desk
        .link
        .as_ref()
        .map(|link| link.workspace.active_id.as_str())
        .unwrap_or_default();
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
    let colors = &look.colors;
    let shape = &look.skin.shape;
    let height = tab_height(look.skin.chrome.titlebar);
    let id = tab.id.clone();
    let tab_selector = SharedString::from(format!("tab-{id}"));
    let close_selector = SharedString::from(format!("tab-close-{id}"));
    let selected = tab.id == active;
    let title = desk::tab_title(desk, tab);
    let agent = strongest_agent(desk, tab);
    let count = tab.zoomed_id.is_some().then_some("⤢");
    let hover = from_hex(&colors.hover);
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
            tab.rounded(px(shape.tab_radius[usize::from(side)]))
                .border_1()
                .border_color(gpui::transparent_black())
                .hover(move |style| style.bg(hover))
        })
        .block_mouse_except_scroll()
        .children(agent.map(|state| {
            div()
                .flex_shrink_0()
                .size(px(7.))
                .rounded_full()
                .bg(from_hex(agent_swatch(state)))
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(selected, |label| {
                    label.font_weight(gpui::FontWeight::MEDIUM)
                })
                .text_color(from_hex(if selected {
                    &colors.theme.foreground
                } else {
                    &colors.subtext
                }))
                .child(title),
        )
        .children(count.map(|count| {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child(count)
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
                .text_color(from_hex(&colors.muted))
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

fn sidebar_column(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .w(px(desk.prefs.sidebar_width as f32))
        .h_full()
        .flex_shrink_0()
        .relative()
        .flex()
        .flex_col()
        .child(sidebar_list(desk, colors, cx))
        .child(sidebar_resize(desk, colors, cx))
}

fn sidebar_list(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
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
    let sessions = session_groups(desk, colors, cx);
    let recent_paths = &desk.recent_projects;
    let recent: Vec<_> = recent_paths
        .iter()
        .enumerate()
        .map(|(index, path)| recent_folder_row(path.clone(), index, colors, cx))
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
        .child(organization_bar(desk, colors, cx))
        .when(!agent_panes.is_empty(), |list| {
            list.child(section_label("Needs you", colors))
                .children(agent_panes.into_iter().map(|pane| {
                    agent_row(desk, pane, agent(&pane.terminal_id), colors, cx).into_any_element()
                }))
                .child(div().h(px(8.)))
        })
        .when(!recent.is_empty(), |list| {
            list.child(section_label("Recent folders", colors))
                .children(recent)
                .child(div().h(px(8.)))
        })
        .when(session_count == 0, |list| {
            list.child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .text_sm()
                    .text_color(from_hex(&colors.muted))
                    .child("No sessions yet."),
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
pub(super) fn main_menu(desk: &Desk, cx: &mut Context<Desk>) -> AnyElement {
    let label = "Phux";
    let menu_ink = desk.look().ink;
    div()
        .flex_1()
        .min_w_0()
        .max_w(px(132.))
        .debug_selector(|| "main-menu-rail".into())
        .occlude()
        .child(
            Button::new("main-menu")
                .label(label)
                .icon(IconName::Search)
                .ghost()
                .small()
                .rounded_full()
                .h(px(30.))
                .px(px(10.))
                .w_full()
                .text_color(menu_ink)
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

fn organization_bar(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
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
            div()
                .flex_1()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child(if busy {
                    "Moving…".to_owned()
                } else if count > 0 {
                    format!("{count} selected")
                } else {
                    String::new()
                }),
        )
        .when(count > 0, |row| {
            row.child(
                Button::new("move-selected-work")
                    .label("Move…")
                    .small()
                    .disabled(desk.organization.busy())
                    .on_click(cx.listener(|desk, _, window, cx| {
                        desk.execute("move-resources", window, cx)
                    })),
            )
        })
        .when(count > 0, |row| {
            row.child(
                Button::new("clear-work-selection")
                    .label("Clear")
                    .ghost()
                    .small()
                    .disabled(desk.organization.busy())
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
                    .disabled(desk.organization.busy())
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
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> AnyElement {
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
            Button::new(SharedString::from(format!("forget-folder-{index}")))
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .tooltip("Forget folder")
                .on_click(cx.listener(move |desk, _, _, cx| {
                    desk.forget_recent_project(&forgotten, cx);
                })),
        )
        .hover(|style| style.bg(from_hex(&colors.hover)))
        .into_any_element()
}

/// One work catalog across authorities. A host is context, never a navigation root.
fn session_groups(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    let mut project = None;
    for work in desk.work_catalog() {
        let group = work
            .project
            .clone()
            .unwrap_or_else(|| (String::new(), "Work".into()));
        if project.as_ref() != Some(&group.0) {
            rows.push(section_label(&group.1, colors).into_any_element());
            project = Some(group.0);
        }
        if desk.work_is_active(&work.destination) {
            rows.push(session_block(
                desk,
                work.id,
                &work.destination.name,
                colors,
                cx,
            ));
        } else {
            rows.push(work_row(work, &desk.skin(), colors, cx));
        }
    }
    for (host, error) in desk.fleet.failures() {
        let name = host.as_deref().unwrap_or("Local");
        rows.push(
            div()
                .id(SharedString::from(format!("unavailable-{name}")))
                .px(px(10.))
                .py(px(6.))
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child(format!("{name} · unavailable"))
                .tooltip(move |window, cx| Tooltip::new(error.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    rows
}

fn work_row(
    work: super::fleet::Work,
    skin: &Skin,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let target = work.destination.clone();
    let tip = format!("{} · Runs on {}", target.name, target.host_label());
    let selector = format!("work-{}-{}", target.host_label(), work.id);
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .min_h(px(52.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(row_radius(skin)))
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .hover(|style| style.bg(from_hex(&colors.hover)))
        .child(
            div()
                .w(px(SIDEBAR_ICON))
                .text_xs()
                .text_color(from_hex(&colors.faint))
                .child("›"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(3.))
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_sm()
                        .child(target.name.clone()),
                )
                .child(host_chip(target.host.as_deref(), work.online, colors)),
        )
        .when(work.needs_you > 0, |row| {
            row.child(pill(
                &work.needs_you.to_string(),
                &colors.accent_wash,
                &colors.theme.accent,
                skin,
            ))
        })
        .opacity(if work.online { 1. } else { 0.55 })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |desk, _, _, cx| desk.navigate_work(target.clone(), cx)))
        .into_any_element()
}

#[cfg(test)]
fn is_intake_session(
    key: Option<SessionKey>,
    awaiting_organization: bool,
    designated: Option<SessionKey>,
) -> bool {
    key.is_some_and(|key| Some(key) == designated) || awaiting_organization
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
    colors: &Palette,
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
    let menu = move |name: String| {
        let handle = handle.clone();
        let rename_target = rename_target.clone();
        move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let rename = handle.clone();
            let kill = handle.clone();
            let select = handle.clone();
            let (rename_name, kill_name) = (name.clone(), name.clone());
            let rename_target = rename_target.clone();
            let menu = if organizing {
                menu.item(PopupMenuItem::new("Select All").on_click(move |_, _, cx| {
                    let _ = select.update(cx, |desk, cx| desk.select_session_work(id, cx));
                }))
            } else {
                menu
            };
            let menu = menu.item(PopupMenuItem::new("Rename Session…").on_click(
                move |_, window, cx| {
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
                },
            ));
            if let Some(key) = intake {
                let organized = handle.clone();
                let intake_name = name.clone();
                menu.item(
                    PopupMenuItem::new("Mark organized").on_click(move |_, _, cx| {
                        let name = intake_name.clone();
                        let _ = organized.update(cx, |desk, cx| desk.resolve_intake(key, name, cx));
                    }),
                )
            } else {
                menu
            }
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
        }
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
    let details = match intake_badge(designated, awaiting) {
        Some("intake") => "Designated home for unorganized work",
        Some(_) => "Imported session awaiting organization",
        None => "Session",
    };
    let session_tip = format!(
        "{name} · Runs on {} · {details} · {} terminals",
        desk.remote.as_deref().unwrap_or("Local"),
        panes.len() + extra.len()
    );
    let header = div()
        .id(SharedString::from(format!("session-{id}")))
        .group("session")
        .min_h(px(52.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(row_radius(&desk.skin())))
        .bg(host_tint(desk.remote.as_deref(), colors).opacity(0.07))
        .cursor_pointer()
        .hover(|style| style.bg(from_hex(&colors.hover)))
        .child(
            div()
                .w(px(SIDEBAR_ICON))
                .text_xs()
                .text_color(from_hex(&colors.faint))
                .child(if collapsed { "▸" } else { "▾" }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(3.))
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(name.to_owned()),
                )
                .child(host_chip(
                    desk.remote.as_deref(),
                    link.connection.status == Status::Attached,
                    colors,
                )),
        )
        .when(
            intake_badge(designated, awaiting) == Some("imported"),
            |row| {
                row.child(pill(
                    "imported",
                    &colors.accent_wash,
                    &colors.theme.accent,
                    &desk.skin(),
                ))
            },
        )
        .when_some(project_path, |header, path| {
            header.child(
                Button::new(SharedString::from(format!("project-open-{id}")))
                    .icon(IconName::Folder)
                    .ghost()
                    .xsmall()
                    .tooltip(format!("Open project window: {}", path.display()))
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
                    Button::new(SharedString::from(format!("session-new-{id}")))
                        .icon(IconName::Plus)
                        .ghost()
                        .xsmall()
                        .tooltip("New terminal in this session")
                        .on_click(cx.listener(move |desk, _, window, cx| {
                            cx.stop_propagation();
                            desk.new_terminal_in(id, window, cx);
                        })),
                ),
        )
        .child(
            Button::new(SharedString::from(format!("session-menu-{id}")))
                .icon(IconName::Ellipsis)
                .accessibility_label("Session actions")
                .ghost()
                .xsmall()
                .tooltip("Session actions")
                .dropdown_menu(menu(name.to_owned())),
        )
        .on_click(
            cx.listener(move |desk, _, _, cx| desk.toggle_session_collapsed(&toggle_name, cx)),
        )
        .when_some(destination, |row, key| {
            let destination_name = name.to_owned();
            let wash = from_hex(&colors.accent_wash);
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
        .context_menu(menu(name.to_owned()));
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
                        .map(|pane| pane_row(desk, pane, colors, cx).into_any_element()),
                )
                .children(
                    extra
                        .iter()
                        .map(|resource| record_row(desk, resource, colors, cx)),
                )
        })
        .into_any_element()
}

/// A record worth a sidebar row: one whose process is coming up, up, or going
/// down. An exited, failed, lost, or never-started record has nothing to show
/// or attach to, so it stays out of the list.
fn running(resource: &ResourceRecord) -> bool {
    matches!(
        resource
            .activation
            .as_ref()
            .map(|activation| activation.state),
        Some(ExecState::Starting | ExecState::Running | ExecState::Stopping)
    )
}

fn record_row(
    desk: &Desk,
    resource: &ResourceRecord,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let key = resource.key;
    let selected = desk.organization.selected.contains(&key);
    let kind = match resource.kind {
        ResourceKind::Terminal => "Terminal",
        _ => "Item",
    };
    let label = resource.name.clone().unwrap_or_else(|| kind.to_owned());
    let state = match resource
        .activation
        .as_ref()
        .map(|activation| activation.state)
    {
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
    div()
        .id(SharedString::from(format!("resource-{key}")))
        .min_h(px(SIDEBAR_ROW))
        .px(px(8.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(row_radius(&desk.skin())))
        .cursor_pointer()
        .bg(if selected {
            from_hex(&colors.active)
        } else {
            gpui::transparent_black()
        })
        .hover(|style| style.bg(from_hex(&colors.hover)))
        .child(
            div()
                .w(px(SIDEBAR_ICON))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(from_hex(&colors.muted)),
                ),
        )
        .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
        .when(state != "Running", |row| {
            row.child(
                div()
                    .text_xs()
                    .text_color(from_hex(&colors.muted))
                    .child(state),
            )
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
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
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
        Some(info) => div()
            .size(px(8.))
            .rounded_full()
            .bg(
                from_hex(agent_swatch(info.state)).opacity(if info.needs_you() {
                    1.
                } else {
                    0.65
                }),
            )
            .into_any_element(),
        None => div()
            .text_xs()
            .text_color(from_hex(if visible {
                &colors.theme.accent
            } else {
                &colors.faint
            }))
            .child("❯")
            .into_any_element(),
    };
    let tooltip_terminal = terminal.clone();
    let handle = cx.weak_entity();
    div()
        .id(SharedString::from(format!("pane-{terminal}")))
        .min_h(px(SIDEBAR_ROW))
        .px(px(8.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(row_radius(&desk.skin())))
        .cursor_pointer()
        .bg(if focused || selected {
            from_hex(&colors.active)
        } else {
            gpui::transparent_black()
        })
        .hover(|style| style.bg(from_hex(&colors.hover)))
        .child(
            div()
                .w(px(SIDEBAR_ICON))
                .flex()
                .justify_center()
                .flex_shrink_0()
                .child(glyph),
        )
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
                    .text_color(from_hex(if focused {
                        &colors.theme.foreground
                    } else {
                        &colors.subtext
                    }))
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
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let terminal = pane.terminal_id.clone();
    let focused = desk.focused_terminal().as_ref() == Some(&terminal);
    let state = if agent.is_some_and(AgentInfo::urgent)
        && agent.is_none_or(|info| info.state != AgentState::Blocked)
    {
        "attention"
    } else {
        agent.map_or("", AgentInfo::word)
    };
    let tint = agent_swatch(agent.map_or(AgentState::Unknown, |info| info.state));
    let urgent = agent.is_some_and(AgentInfo::urgent);
    let tooltip_terminal = terminal.clone();
    let handle = cx.weak_entity();
    div()
        .id(SharedString::from(format!("agent-{terminal}")))
        .min_h(px(SIDEBAR_ROW))
        .px(px(8.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(row_radius(&desk.skin())))
        .border_1()
        .border_color(if urgent {
            from_hex(&format!("{tint}66"))
        } else {
            gpui::transparent_black()
        })
        .bg(if focused {
            from_hex(&colors.active)
        } else if urgent {
            from_hex(&format!("{tint}14"))
        } else {
            gpui::transparent_black()
        })
        .cursor_pointer()
        .hover(|style| style.bg(from_hex(&colors.hover)))
        .child(
            div()
                .w(px(SIDEBAR_ICON))
                .flex()
                .justify_center()
                .child(div().size(px(8.)).rounded_full().bg(from_hex(tint))),
        )
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

fn sidebar_resize(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let active = desk.dragging_sidebar();
    let accent = from_hex(&colors.theme.accent);
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
            column.child(status_bar(desk, status, &look.skin, &look.colors, cx))
        })
}

fn workspace_body(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let colors = &look.colors;
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
            zoomed_pane(desk, &id, window, cx).into_any_element()
        } else if let Some(root) = root {
            split_tree(desk, &root, look, window, cx).into_any_element()
        } else {
            empty_state(desk, colors, cx).into_any_element()
        })
        .children(peek_card(desk, look, cx))
}

fn empty_state(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.link.as_ref().map(|link| link.connection.status);
    let error = desk.connect_error.clone().or_else(|| {
        desk.link
            .as_ref()
            .and_then(|link| link.connection.error.clone())
    });
    let pending = desk
        .link
        .as_ref()
        .map(|link| link.workspace.pending())
        .unwrap_or(0)
        > 0;
    let muted = from_hex(&colors.muted);
    let body = match status {
        Some(Status::Attached) => div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.))
            .child(empty_action(
                desk,
                "empty-new",
                if pending {
                    "Starting terminal…"
                } else {
                    "New terminal"
                },
                "new",
                true,
                cx,
            ))
            .child(empty_action(
                desk,
                "empty-folder",
                "Open folder…",
                "folder",
                false,
                cx,
            ))
            .child(empty_action(
                desk,
                "empty-palette",
                "Command palette",
                "palette",
                false,
                cx,
            )),
        Some(Status::Idle | Status::Connecting | Status::Negotiated) => div()
            .text_sm()
            .text_color(muted)
            .child(match desk.remote.as_deref() {
                Some(host) => format!("Connecting to {host}…"),
                None => format!("Connecting to {}…", desk.socket.display()),
            }),
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
                    .text_color(from_hex(&colors.theme.danger))
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
) -> impl IntoElement {
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
    let chord = desk.bound_chord(id)?;
    let keystroke = Keystroke::parse(&gpui_stroke(chord)?).ok()?;
    Some(Kbd::new(keystroke))
}

fn zoomed_pane(
    desk: &mut Desk,
    id: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let presence = desk.motion.presence(id, Instant::now());
    desk.placement(id)
        .cloned()
        .map(|placement| pane_view(desk, &placement, presence, window, cx))
        .unwrap_or_else(|| div().into_any_element())
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
        LayoutNode::Leaf { placement } => {
            let presence = desk.motion.presence(&placement.id, Instant::now());
            pane_view(desk, placement, presence, window, cx).into_any_element()
        }
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
    let accent = from_hex(&look.colors.theme.accent);
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
    presence: f32,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let focused =
        desk.focused_placement().map(|item| item.id.as_str()) == Some(placement.id.as_str());
    let look = desk.look();
    let colors = &look.colors;
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
    let leaves = tab
        .map(|tab| layout::placements(&tab.root).len())
        .unwrap_or(1);
    let find = desk
        .find
        .as_ref()
        .filter(|find| find.placement_id == placement.id)
        .map(|find| find.case_sensitive);
    let find_bar = find.map(|case| overlays::find_bar(desk, case, colors, cx));
    let dim = (!focused && leaves > 1 && !zoomed)
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
    let accent = from_hex(&colors.theme.accent);
    let edge = if dropping {
        accent
    } else if urgent {
        agent
            .as_ref()
            .map(|info| from_hex(agent_swatch(info.state)).opacity(0.75))
            .unwrap_or(look.lit)
    } else if focused && leaves > 1 && !zoomed {
        look.lit
    } else {
        look.edge
    };
    let shine = look
        .lit
        .opacity(if focused { 0.7 } else { 0.35 } * gloss(&look.skin));
    let radius = look.skin.shape.radius;
    let controls_bar = if controls {
        Some(pane_controls(desk, placement, zoomed, &look, cx).into_any_element())
    } else {
        None
    };
    let conversation_toggle =
        desk.conversation_eligible(&id) && !desk.conversation_visible(&id) && !controls;
    let conversation_id = id.clone();
    let drag_id = id.clone();
    let drop_id = id.clone();
    let exit_id = id.clone();
    let hover_id = id.clone();
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
        .children(specular(shine, radius))
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
                .on_hover(cx.listener(move |desk, hovered: &bool, _, cx| {
                    if *hovered {
                        desk.chrome_hover = Some(hover_id.clone());
                    } else if desk.chrome_hover.as_deref() == Some(hover_id.as_str()) {
                        desk.chrome_hover = None;
                    }
                    cx.notify();
                })),
        )
        .when(fenced && !controls, |card| {
            card.child(div().absolute().top(px(8.)).right(px(10.)).child(pill(
                "Typing may not have arrived",
                &format!("{}22", colors.theme.warning),
                &colors.theme.warning,
                &look.skin,
            )))
        })
        .children(controls_bar)
        .when(conversation_toggle, |card| {
            card.child(
                div().absolute().top(px(8.)).right(px(10.)).child(
                    Button::new(SharedString::from(format!("conversation-open-{id}")))
                        .label("Conversation")
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |desk, _, window, cx| {
                            desk.toggle_conversation(&conversation_id, window, cx);
                        })),
                ),
            )
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
    let colors = &look.colors;
    let title = desk.pane_title(placement.terminal_id.as_ref());
    let agent = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.agent(id))
        .cloned();
    let id = placement.id.clone();
    let hover_id = placement.id.clone();
    let focus_id = placement.id.clone();
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
        .on_hover(cx.listener(move |desk, hovered: &bool, _, cx| {
            if *hovered {
                desk.controls_hover = Some(hover_id.clone());
            } else if desk.controls_hover.as_deref() == Some(hover_id.as_str()) {
                desk.controls_hover = None;
            }
            cx.notify();
        }))
        .children(agent.as_ref().map(|info| {
            div()
                .flex_shrink_0()
                .size(px(7.))
                .rounded_full()
                .bg(from_hex(agent_swatch(info.state)))
        }))
        .child(
            div()
                .flex_shrink_0()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(from_hex(&colors.theme.foreground))
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
                Button::new(SharedString::from(format!("pane-conversation-{id}")))
                    .label(if desk.conversation_visible(&id) {
                        "Terminal"
                    } else {
                        "Conversation"
                    })
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |desk, _, window, cx| {
                        cx.stop_propagation();
                        desk.toggle_conversation(&conversation_id, window, cx);
                    })),
            )
        })
        .child(header_button(
            &id,
            "zoom",
            if zoomed {
                IconName::Minimize
            } else {
                IconName::Maximize
            },
            if zoomed { "Restore" } else { "Zoom" },
            on_pane(id.clone(), "zoom"),
            cx,
        ))
        .child({
            let handle = cx.weak_entity();
            let placement = id.clone();
            let open_handle = handle.clone();
            let open_placement = placement.clone();
            let selector = id.clone();
            div()
                .debug_selector(move || format!("pane-actions-{selector}"))
                .child(
                    Button::new(SharedString::from(format!("pane-more-{id}")))
                        .icon(IconName::Ellipsis)
                        .accessibility_label("Pane actions")
                        .ghost()
                        .xsmall()
                        .tooltip("Pane actions")
                        .dropdown_menu(move |mut menu, _, _| {
                            for (command, label) in [
                                ("split-right", "Split right"),
                                ("split-down", "Split down"),
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
    let colors = &look.colors;
    let accent = from_hex(&colors.theme.accent);
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
            div()
                .px(px(16.))
                .py(px(10.))
                .surface(look, Surface::Card)
                .rounded(px(look.skin.shape.card_radius - 2.))
                .bg(from_hex(&colors.raised).opacity(0.92))
                .when(!look.popover_shadow.is_empty(), |hint| hint.shadow_lg())
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(from_hex(&colors.theme.foreground))
                        .child(if remote {
                            "Drop to upload"
                        } else {
                            "Drop to attach"
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(from_hex(&colors.muted))
                        .child(if remote {
                            "Sent to that machine, then its path is pasted"
                        } else {
                            "Paths paste here, ready for your agent"
                        }),
                ),
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

fn header_button(
    placement: &str,
    name: &str,
    icon: IconName,
    tooltip: &str,
    run: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    Button::new(SharedString::from(format!("{name}-{placement}")))
        .icon(icon)
        .ghost()
        .xsmall()
        .accessibility_label(tooltip.to_owned())
        .tooltip(tooltip.to_owned())
        .on_click(cx.listener(move |desk, _, window, cx| {
            cx.stop_propagation();
            run(desk, window, cx);
        }))
}

/// Exceptions only: an unhealthy connection, agents that need a person, a
/// terminal still starting, and a pane scrolled away from live output.
fn status_bar(
    desk: &Desk,
    height: f32,
    skin: &Skin,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let chip = chip_radius(skin);
    let (label, tone) = desk.connection_label();
    let unhealthy = desk.unhealthy();
    let scrolled = desk.session_is_attached()
        && desk
            .focused_view()
            .and_then(|view| desk.client().and_then(|client| client.acquire_view(view)))
            .is_some_and(|frame| !frame.scrollbar.at_tail());
    let pending = desk
        .link
        .as_ref()
        .map(|link| link.workspace.pending())
        .unwrap_or(0);
    let waiting = desk
        .link
        .as_ref()
        .map(|link| {
            link.connection
                .agents
                .values()
                .filter(|info| info.needs_you())
                .count()
        })
        .unwrap_or(0);
    let tone_color = from_hex(match tone {
        Tone::Good => &colors.theme.success,
        Tone::Busy => &colors.theme.warning,
        Tone::Bad => &colors.theme.danger,
    });
    let hover = from_hex(&colors.hover);
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
                div()
                    .id("status-connection")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(4.))
                    .rounded(px(chip))
                    .child(div().size(px(7.)).rounded_full().bg(tone_color))
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
                div()
                    .id("status-agents")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(4.))
                    .rounded(px(chip))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(from_hex(agent_swatch(AgentState::Blocked))),
                    )
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
                div()
                    .id("status-scrolled")
                    .px(px(6.))
                    .rounded(px(chip))
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
fn bell(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let unread: Vec<&Notification> = desk.feed.unread().collect();
    let badge = unread.first().map(|_| {
        let tint = unread
            .iter()
            .find(|entry| entry.kind == Kind::Error)
            .or(unread.last())
            .map(|entry| notification_edge(entry, colors))
            .unwrap_or_else(|| colors.theme.accent.clone());
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
            .bg(from_hex(&tint))
            .text_color(from_hex(&colors.theme.background))
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
pub(super) fn notification_edge(entry: &Notification, colors: &Palette) -> String {
    match entry.kind {
        Kind::Error => colors.theme.danger.clone(),
        Kind::Agent => entry
            .agent_state
            .map(agent_swatch)
            .map(str::to_owned)
            .unwrap_or_else(|| colors.theme.warning.clone()),
        Kind::Approval => colors.theme.warning.clone(),
        Kind::Info | Kind::Refusal => colors.theme.accent.clone(),
    }
}

/// The newest agent or error entry, briefly, over the workspace. Everything
/// else waits behind the bell.
fn peek_card(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> Option<impl IntoElement> {
    let colors = &look.colors;
    if desk.modal == Modal::Notifications {
        return None;
    }
    let entry = desk.feed.peek()?;
    let id = entry.id;
    let more = desk.feed.unread().count().saturating_sub(1);
    Some(
        div()
            .id(("notification-peek", id as usize))
            .absolute()
            .bottom(px(12.))
            .right(px(12.))
            .w(px(340.))
            .p(px(10.))
            .pl(px(12.))
            .surface(look, Surface::Card)
            .bg(from_hex(&colors.raised).opacity(0.94))
            .border_l_4()
            .border_color(from_hex(&notification_edge(entry, colors)))
            .when(!look.popover_shadow.is_empty(), |card| card.shadow_lg())
            .occlude()
            .cursor_pointer()
            .flex()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(from_hex(&colors.theme.foreground))
                            .child(entry.title.clone()),
                    )
                    .children(entry.body.clone().map(|body| {
                        div()
                            .text_xs()
                            .line_clamp(3)
                            .text_color(from_hex(&colors.muted))
                            .child(body)
                    }))
                    .when(more > 0, |text| {
                        text.child(
                            div()
                                .text_xs()
                                .text_color(from_hex(&colors.subtext))
                                .child(format!("+{more} unread alerts")),
                        )
                    }),
            )
            .child(
                Button::new(("notification-peek-dismiss", id as usize))
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .tooltip("Dismiss")
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

fn section_label(title: &str, colors: &Palette) -> gpui::Div {
    div()
        .px(px(8.))
        .pt(px(8.))
        .h(px(28.))
        .flex()
        .items_center()
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(from_hex(&colors.muted))
        .child(div().flex_1().child(title.to_owned()))
}

fn pill(text: &str, background: &str, color: &str, skin: &Skin) -> gpui::Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .rounded(px(skin.shape.kit_radius))
        .bg(from_hex(background))
        .text_xs()
        .text_color(from_hex(color))
        .child(text.to_owned())
}

/// A chrome icon button that keeps the titlebar from dragging the window when pressed.
fn icon_button(
    id: &str,
    icon: IconName,
    label: &str,
    chord: Option<String>,
    click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let tooltip = match chord {
        Some(chord) => format!("{label}  {chord}"),
        None => label.to_owned(),
    };
    div().occlude().child(
        Button::new(id.to_owned())
            .icon(icon)
            .accessibility_label(label.to_owned())
            .ghost()
            .small()
            .tooltip(tooltip)
            .on_click(click),
    )
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
pub(super) fn agent_swatch(state: AgentState) -> &'static str {
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
    use super::{intake_badge, is_intake_session};
    use phux_protocol::ids::SessionKey;

    #[test]
    fn designated_intake_is_grouped_even_when_not_imported() {
        let intake = SessionKey::new([1; 16]).unwrap();
        let named = SessionKey::new([2; 16]).unwrap();
        assert!(is_intake_session(Some(intake), false, Some(intake)));
        assert!(is_intake_session(Some(named), true, Some(intake)));
        assert!(!is_intake_session(Some(named), false, Some(intake)));
        assert!(!is_intake_session(None, false, None));
        assert!(!is_intake_session(Some(named), false, None));
    }

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
