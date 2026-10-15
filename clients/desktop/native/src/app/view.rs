//! Terminal-first chrome: titlebar, sidebar, split tree, status, overlays.
//!
//! The window is one sheet of glass over the desktop blur. Titlebar, sidebar
//! and status sit on it unframed; panes float on it as rounded cards with no
//! header. A pane's controls appear when the pointer reaches its top edge.
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
use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor, Status};
use phux_protocol::ids::{ResourceKey, SessionKey};
use phux_protocol::wire::frame::{ExecState, ResourceRecord};
use phux_protocol::{ResourceId, ResourceKind};

use crate::connection::{AgentInfo, AgentState, state_rank};
use crate::settings::theme::Palette;
use crate::workspace::layout::{self, Axis, DeskTab, LayoutNode, Placement};
use crate::workspace::place::{self, SessionLine};

use super::actions::{OpenPathDirectory, Run};
use super::color::from_hex;
use super::desk::{self, Desk, Modal, Tone};
use super::host;
use super::keys::gpui_stroke;
use super::motion;
use super::notifications::{Kind, Notification};
use super::overlays;

const TITLEBAR: f32 = 44.;
const STATUS: f32 = 26.;
/// The glass between panes, which is also the split's grab area.
const GAP: f32 = 12.;
/// The glass around the pane canvas.
const GUTTER: f32 = 12.;
const RADIUS: f32 = 12.;
const SIDEBAR_ROW: f32 = 32.;
const SIDEBAR_ICON: f32 = 16.;
/// How far down a pane the pointer reveals its controls.
const CONTROLS_EDGE: f32 = 36.;
/// Left inset that clears the traffic lights.
const TRAFFIC_LIGHTS: f32 = 78.;
/// Room for the host-labelled main menu and sidebar toggle, even with the
/// navigator hidden. Long host names truncate instead of pushing tabs away.
const MENU_RAIL: f32 = 192.;

/// Paint values for the glass sheet and the cards on it, from the theme.
struct Glass {
    /// The window sheet over the desktop blur.
    sheet: Hsla,
    /// Light falling from the top edge of the sheet.
    sheen: Hsla,
    /// A pane card: the terminal background at the terminal opacity.
    card: Hsla,
    /// Neutral and focused edges. Quiet cards keep their edge transparent.
    edge: Hsla,
    lit: Hsla,
    shadow: Hsla,
}

impl Glass {
    fn of(colors: &Palette, terminal_opacity: f64) -> Self {
        let light = colors.theme.appearance == "light";
        let ink = from_hex(&colors.theme.foreground);
        Self {
            sheet: from_hex(&colors.theme.background).opacity(if light { 0.58 } else { 0.44 }),
            sheen: ink.opacity(if light { 0.07 } else { 0.05 }),
            card: from_hex(&crate::settings::theme::with_alpha(
                &colors.theme.background,
                terminal_opacity,
            )),
            edge: gpui::transparent_black(),
            lit: ink.opacity(0.22),
            shadow: gpui::black().opacity(if light { 0.08 } else { 0.18 }),
        }
    }
}

/// The palette with translucent washes and hairlines, so hovered and
/// selected chrome tints the glass instead of covering it.
fn chrome_palette(desk: &Desk) -> Palette {
    let mut colors = desk.palette();
    let ink = colors.theme.foreground.clone();
    let alpha = crate::settings::theme::with_alpha;
    colors.hover = alpha(&ink, 0.06);
    colors.active = alpha(&ink, 0.11);
    colors.border = alpha(&ink, 0.08);
    colors.divider = alpha(&ink, 0.08);
    colors
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
    let colors = chrome_palette(desk);
    titlebar(desk, sidebar, &colors, cx).into_any_element()
}

fn build_sidebar(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let colors = chrome_palette(desk);
    sidebar_column(desk, &colors, cx).into_any_element()
}

fn build_overlays(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let colors = chrome_palette(desk);
    overlays::layer(desk, &colors, window, cx)
}

pub fn render(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    let _frame = crate::diag::FRAME.timer();
    let colors = chrome_palette(desk);
    let glass = Glass::of(&colors, desk.prefs.terminal_opacity);
    let sidebar = desk.prefs.sidebar_visible && desk.role != desk::WindowRole::Quick;
    desk.set_viewport(window.viewport_size());
    desk.sync_title(window);
    desk.settle_focus(window, cx);
    desk.ensure_clock(cx);
    desk.flush_system(cx);
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

    div()
        .id("desk")
        .key_context("Desk")
        .track_focus(&desk.focus)
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(glass.sheet)
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(220.))
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(glass.sheen, 0.),
                    linear_color_stop(glass.sheen.opacity(0.), 1.),
                )),
        )
        .text_color(from_hex(&colors.theme.foreground))
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
            StyleRefinement::default().w_full().h(px(TITLEBAR)),
            cx,
        ))
        .child(
            div()
                .flex_grow(1.)
                .flex()
                .min_h(px(0.))
                .when(sidebar, |row| {
                    let width = px(desk.prefs.sidebar_width as f32);
                    row.child(chrome_slot(
                        &mut desk.chrome.sidebar,
                        build_sidebar,
                        StyleRefinement::default().w(width).h_full(),
                        cx,
                    ))
                })
                .child(main_column(desk, &colors, window, cx)),
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
    let (tab, placements) = desk
        .active_tab()
        .map(|tab| {
            let ids = layout::placements(&tab.root)
                .into_iter()
                .map(|placement| placement.id.clone())
                .collect::<Vec<_>>();
            (Some(tab.id.clone()), ids)
        })
        .unwrap_or_default();
    desk.motion.observe(tab.as_deref(), &placements, now);
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

fn titlebar(
    desk: &mut Desk,
    sidebar: bool,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let region = desk
        .drag_region
        .get_or_insert_with(|| cx.new(crate::chrome::DragRegion::new))
        .clone();
    let tabs = desk
        .link
        .as_ref()
        .map(|link| link.workspace.tabs.clone())
        .unwrap_or_default();
    let active = desk
        .link
        .as_ref()
        .map(|link| link.workspace.active_id.clone())
        .unwrap_or_default();
    let centers = desk.tab_centers.clone();
    let quick = desk.role == desk::WindowRole::Quick;
    let lead = titlebar_lead(sidebar, quick, desk.prefs.sidebar_width as f32);
    div()
        .h(px(TITLEBAR))
        .w_full()
        .relative()
        .flex()
        .items_center()
        .flex_shrink_0()
        .child(div().absolute().inset_0().child(region))
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
                .child(main_menu(desk, cx))
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
                        cx.listener(|desk, _, window, cx| desk.execute("sidebar", window, cx)),
                    ))
                })
                .with_spring(
                    "main-menu-rail",
                    gpui::SpringAnimation::new(gpui::SpringConfig::new(600., 49., 1.)).to(lead),
                    |rail, width| rail.w(px(width)),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(4.))
                .overflow_x_hidden()
                .children(
                    tabs.iter()
                        .map(|tab| tab_button(desk, tab, &active, colors, cx).into_any_element()),
                )
                .on_children_prepainted(move |bounds, _, _| {
                    *centers.borrow_mut() = bounds
                        .iter()
                        .map(|bounds| (bounds.origin.x + bounds.size.width / 2.).as_f32())
                        .collect();
                }),
        )
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(2.))
                .pr(px(8.))
                .child(icon_button(
                    "new-tab",
                    IconName::Plus,
                    "New Terminal",
                    desk.shown_chord("new"),
                    cx.listener(|desk, _, window, cx| desk.execute("new", window, cx)),
                ))
                .child(bell(desk, colors, cx))
                .child(update_button(desk, cx)),
        )
}

fn update_button(desk: &Desk, cx: &mut Context<Desk>) -> AnyElement {
    let entity = desk.updates.clone();
    let updater = entity.read(cx);
    if !updater.desktop_available() || updater.development {
        return div().into_any_element();
    }
    let busy = updater.busy;
    Button::new("desktop-update-ready")
        .label(if busy {
            "Updating…"
        } else {
            "Update & restart"
        })
        .small()
        .disabled(busy)
        .on_click(move |_, _, cx| entity.update(cx, |updater, cx| updater.install_desktop(cx)))
        .into_any_element()
}

fn tab_button(
    desk: &Desk,
    tab: &DeskTab,
    active: &str,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let id = tab.id.clone();
    let selected = tab.id == active;
    let title = desk::tab_title(desk, tab);
    let agent = strongest_agent(desk, tab);
    let count = tab.zoomed_id.is_some().then_some("⤢");
    let hover = from_hex(&colors.hover);
    let close_id = tab.id.clone();
    let drag_id = tab.id.clone();
    let rename_id = tab.id.clone();
    let middle_id = tab.id.clone();
    div()
        .id(SharedString::from(format!("tab-{id}")))
        .group("tab")
        .h(px(28.))
        .min_w(px(88.))
        .max_w(px(220.))
        .flex_1()
        .pl(px(10.))
        .pr(px(4.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(9.))
        .bg(if selected {
            from_hex(&colors.active)
        } else {
            gpui::transparent_black()
        })
        .when(!selected, |tab| tab.hover(move |style| style.bg(hover)))
        .occlude()
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
                .id(SharedString::from(format!("tab-close-{id}")))
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
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |desk, _, window, cx| {
                        desk.close_tab(&close_id);
                        desk.after_command(window, cx);
                        cx.stop_propagation();
                    }),
                ),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |desk, event: &gpui::MouseDownEvent, window, cx| {
                desk.select_tab(&drag_id);
                desk.begin_tab_drag(drag_id.clone(), event.position.x.as_f32(), cx);
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
    let agents = link
        .map(|link| link.connection.agents.clone())
        .unwrap_or_default();
    let panes = link
        .map(|link| link.connection.panes().to_vec())
        .unwrap_or_default();
    let mut agent_panes: Vec<_> = panes
        .iter()
        .filter(|pane| {
            agents
                .get(&pane.terminal_id)
                .is_some_and(AgentInfo::needs_you)
        })
        .cloned()
        .collect();
    agent_panes.sort_by(|left, right| {
        let (left, right) = (
            agents.get(&left.terminal_id),
            agents.get(&right.terminal_id),
        );
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
        .py(px(GUTTER))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(organization_bar(desk, colors, cx))
        .when(!agent_panes.is_empty(), |list| {
            list.child(section_label("Needs you", colors))
                .children(agent_panes.iter().map(|pane| {
                    agent_row(desk, pane, agents.get(&pane.terminal_id), colors, cx)
                        .into_any_element()
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
        .child(hosts_section(desk, colors, cx))
}

/// Registered machines from `phux host ls`: switch this window to one, add one
/// over ssh, or forget one. Always shown, so adding a host is never hidden. A
/// plain click points this window at the host; its context menu still opens a
/// separate window. The filled dot marks the host this window is on.
fn hosts_section(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    let header = section_label("Hosts", colors).child(
        Button::new("host-add")
            .icon(IconName::Plus)
            .ghost()
            .xsmall()
            .tooltip("Add a host over ssh")
            .disabled(desk.host_adding.is_some())
            .on_click(
                cx.listener(|desk, _, window, cx| desk.open_modal(Modal::AddHost, window, cx)),
            ),
    );
    let mut section = div().flex().flex_col().gap(px(1.)).pt(px(8.)).child(header);
    for entry in &desk.hosts {
        let here = desk.remote.as_deref() == Some(entry.name.as_str());

        section = section.child(host_row(entry, here, colors, cx));
    }
    if let Some(target) = desk.host_adding.as_deref() {
        section = section.child(
            div()
                .px(px(10.))
                .py(px(6.))
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child(format!("Adding {target}…")),
        );
    }
    section.into_any_element()
}

fn host_row(
    entry: &host::RemoteHost,
    here: bool,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let name = entry.name.clone();
    let handle = cx.weak_entity();
    let menu = {
        let name = name.clone();
        move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let (open, remove) = (handle.clone(), handle.clone());
            let (open_name, remove_name) = (name.clone(), name.clone());
            menu.item(PopupMenuItem::new("Open Window").on_click(move |_, _, cx| {
                let name = open_name.clone();
                let _ = open.update(cx, |desk, cx| desk.open_host(name, cx));
            }))
            .item(
                PopupMenuItem::new("Remove Host…").on_click(move |_, window, cx| {
                    let name = remove_name.clone();
                    let _ = remove.update(cx, |desk, cx| {
                        desk.open_modal(Modal::RemoveHost { name }, window, cx)
                    });
                }),
            )
        }
    };
    let click = name.clone();
    div()
        .id(SharedString::from(format!("host-{name}")))
        .min_h(px(SIDEBAR_ROW))
        .px(px(8.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .bg(if here {
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
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .when(here, |dot| dot.bg(from_hex(&colors.theme.accent)))
                        .when(!here, |dot| {
                            dot.border_1().border_color(from_hex(&colors.faint))
                        }),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .truncate()
                .child(name.clone()),
        )
        .child(
            Button::new(SharedString::from(format!("host-menu-{name}")))
                .icon(IconName::Ellipsis)
                .ghost()
                .xsmall()
                .tooltip("Host actions")
                .dropdown_menu(menu.clone()),
        )
        .on_click(cx.listener(move |desk, _, _, cx| desk.switch_host(Some(click.clone()), cx)))
        .tooltip({
            let endpoint = entry.endpoint.clone();
            move |window, cx| Tooltip::new(endpoint.clone()).build(window, cx)
        })
        .context_menu(menu)
        .into_any_element()
}

fn titlebar_lead(sidebar: bool, quick: bool, sidebar_width: f32) -> f32 {
    if quick {
        return MENU_RAIL - 28.;
    }
    if sidebar { sidebar_width } else { MENU_RAIL }
}

/// A stable entry point; the material and interaction live in their own window.
pub(super) fn main_menu(desk: &Desk, cx: &mut Context<Desk>) -> AnyElement {
    let label = desk.remote.clone().unwrap_or_else(|| "Local".to_owned());
    div()
        .flex_1()
        .min_w_0()
        .max_w(px(132.))
        .debug_selector(|| "main-menu-rail".into())
        .occlude()
        .child(
            Button::new("main-menu")
                .label(label)
                .dropdown_caret(true)
                .ghost()
                .small()
                .rounded_full()
                .h(px(30.))
                .px(px(10.))
                .w_full()
                .on_click(cx.listener(|desk, _, window, cx| {
                    super::glass_menu::toggle(desk, window, cx);
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

/// Sessions, home first, grouped by typed project identity and remote host.
fn session_groups(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> Vec<AnyElement> {
    let Some(link) = desk.link.as_ref() else {
        return Vec::new();
    };
    let Some(topology) = link.connection.topology.as_ref() else {
        return Vec::new();
    };
    let mut sessions: Vec<_> = topology.sessions.iter().collect();
    let mut intake: Vec<_> = sessions
        .iter()
        .copied()
        .filter(|session| {
            is_intake_session(session.key, session.awaiting_organization, topology.inbox)
        })
        .collect();
    intake.sort_by_key(|session| session.key != topology.inbox);
    sessions.retain(|session| {
        !is_intake_session(session.key, session.awaiting_organization, topology.inbox)
    });
    let home = desk.session.as_str();
    sessions.sort_by(|left, right| {
        (left.name != home)
            .cmp(&(right.name != home))
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut rows = Vec::new();
    if !intake.is_empty() {
        rows.push(section_label("Intake sessions", colors).into_any_element());
        for session in intake {
            rows.push(session_block(desk, session.id, &session.name, colors, cx));
        }
    }
    rows.extend(named_session_rows(desk, &sessions, colors, cx));
    rows
}

fn named_session_rows(
    desk: &Desk,
    sessions: &[&SessionDescriptor],
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> Vec<AnyElement> {
    let Some(link) = desk.link.as_ref() else {
        return Vec::new();
    };
    let panes = link.connection.panes();
    let lines: Vec<SessionLine> = sessions
        .iter()
        .map(|session| SessionLine {
            name: session.name.clone(),
            host: session_host(panes, session.id),
            project: link.connection.projects.get(&session.id).cloned(),
        })
        .collect();
    let grouped =
        !link.connection.projects.is_empty() || lines.iter().any(|line| line.host.is_some());
    let mut rows = Vec::new();
    if !grouped && !sessions.is_empty() {
        rows.push(section_label("Sessions", colors).into_any_element());
    }
    for group in place::group_by_project(&lines) {
        if grouped {
            // Ungrouped sessions sort last; "Sessions" keeps them out of the group above.
            let title = session_group_title(group.project.as_deref(), group.host.as_deref());
            rows.push(section_label(&title, colors).into_any_element());
        }
        for line in group.sessions {
            if let Some(session) = sessions.iter().find(|session| session.name == line.name) {
                rows.push(session_block(desk, session.id, &session.name, colors, cx));
            }
        }
    }
    rows
}

fn session_group_title(project: Option<&str>, host: Option<&str>) -> String {
    match (project, host) {
        (Some(project), Some(host)) => format!("{project} · {host}"),
        (Some(project), None) => project.to_owned(),
        (None, Some(host)) => host.to_owned(),
        (None, None) => "Sessions".to_owned(),
    }
}

fn session_host(panes: &[PaneDescriptor], session: u32) -> Option<String> {
    let mut hosts: Vec<String> = panes
        .iter()
        .filter(|pane| pane.session_id == session)
        .filter_map(|pane| place::terminal_host(Some(&pane.terminal_id.to_string())))
        .collect();
    hosts.dedup();
    // A session spread over several hosts belongs to none of them.
    if hosts.len() == 1 { hosts.pop() } else { None }
}

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
                    PopupMenuItem::new("Mark session organized").on_click(move |_, _, cx| {
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
        "{name} · {details} · {} terminals",
        panes.len() + extra.len()
    );
    let header = div()
        .id(SharedString::from(format!("session-{id}")))
        .group("session")
        .h(px(SIDEBAR_ROW))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(6.))
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
                .truncate()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(name.to_owned()),
        )
        .when(awaiting && !designated, |row| {
            row.child(pill("imported", &colors.accent_wash, &colors.theme.accent))
        })
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
        .rounded(px(6.))
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
        .rounded(px(6.))
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
        .rounded(px(6.))
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

/// Whether the sidebar label for `then` changes between `earlier` and `now`.
pub(super) fn ago_changed(then: u64, earlier: u64, now: u64) -> bool {
    ago(then, earlier) != ago(then, now)
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
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    div()
        .flex_grow(1.)
        .min_w(px(0.))
        .flex()
        .flex_col()
        .child(workspace_body(desk, colors, window, cx))
        .child(status_bar(desk, colors, cx))
}

fn workspace_body(
    desk: &mut Desk,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let zoomed = desk.active_tab().and_then(|tab| tab.zoomed_id.clone());
    let root = desk.active_tab().map(|tab| tab.root.clone());
    div()
        .flex_grow(1.)
        .min_h(px(0.))
        .relative()
        .pt(px(GUTTER))
        .px(px(GUTTER))
        .child(if let Some(id) = zoomed {
            zoomed_pane(desk, &id, window, cx).into_any_element()
        } else if let Some(root) = root {
            split_tree(desk, &root, colors, window, cx).into_any_element()
        } else {
            empty_state(desk, colors, cx).into_any_element()
        })
        .children(peek_card(desk, colors, cx))
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
    let chord = settings_chord(desk, id)?;
    let keystroke = Keystroke::parse(&gpui_stroke(&chord)?).ok()?;
    Some(Kbd::new(keystroke))
}

fn settings_chord(desk: &Desk, id: &str) -> Option<String> {
    crate::settings::shown_chords(desk.keymap(), &crate::settings::default_chords())
        .into_iter()
        .find(|(command, _)| command == id)
        .map(|(_, chord)| chord)
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
    colors: &Palette,
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
            let gap = GAP * lead.min(trail);
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
                        .child(split_tree(desk, first, colors, window, cx)),
                )
                .child(split_divider(desk, id.clone(), *axis, gap, colors, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(split_tree(desk, second, colors, window, cx)),
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

/// The glass between two panes is the grab area. A short pill appears on
/// hover, in the accent while dragged.
fn split_divider(
    desk: &Desk,
    id: String,
    axis: Axis,
    gap: f32,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let horizontal = matches!(axis, Axis::Row);
    let active = desk.dragging_split() == Some(id.as_str());
    let accent = from_hex(&colors.theme.accent);
    let grip = from_hex(&colors.theme.foreground).opacity(0.28);
    div()
        .id(SharedString::from(format!("split-{id}")))
        .group("divider")
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .when(horizontal, |bar| {
            bar.w(px(gap)).h_full().cursor_col_resize()
        })
        .when(!horizontal, |bar| {
            bar.h(px(gap)).w_full().cursor_row_resize()
        })
        .child(
            div()
                .rounded_full()
                .when(active, |pill| pill.bg(accent))
                .when(!active, |pill| {
                    pill.group_hover("divider", move |style| style.bg(grip))
                })
                .when(horizontal, |pill| pill.w(px(3.)).h(px(44.)))
                .when(!horizontal, |pill| pill.h(px(3.)).w(px(44.))),
        )
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
    let colors = chrome_palette(desk);
    let glass = Glass::of(&colors, desk.prefs.terminal_opacity);
    let placement_id = placement.id.clone();
    let terminal = placement.terminal_id.clone();
    let view = placement.view_id;
    // The terminal holds the keyboard only while no modal or find field does.
    let settings = desk.terminal_settings(placement, focused && !desk.text_field_has_keys());
    let client = desk.client();
    let body = if desk.conversation_visible(&placement_id) {
        super::conversation::render(desk, &placement_id, &colors, window, cx)
    } else if let (Some(terminal), Some(view), Some(settings), Some(client)) =
        (terminal, view, settings, client)
    {
        let entity = desk.panes_mut().entity_for(
            &placement_id,
            &terminal,
            view,
            settings,
            &client,
            window,
            cx,
        );
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
    let find_bar = find.map(|case| overlays::find_bar(desk, case, &colors, cx));
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
    let leaving = desk.motion.is_leaving(&id);
    let controls = find.is_none()
        && !dropping
        && !leaving
        && desk.dragging_split().is_none()
        && (desk.chrome_hover.as_deref() == Some(id.as_str())
            || desk.controls_hover.as_deref() == Some(id.as_str()));
    let accent = from_hex(&colors.theme.accent);
    let edge = if dropping {
        accent
    } else if urgent {
        agent
            .as_ref()
            .map(|info| from_hex(agent_swatch(info.state)).opacity(0.75))
            .unwrap_or(glass.lit)
    } else if focused && leaves > 1 && !zoomed {
        glass.lit
    } else {
        glass.edge
    };
    let controls_bar = if controls {
        Some(pane_controls(desk, placement, zoomed, &colors, cx).into_any_element())
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
        .rounded(px(RADIUS))
        .bg(glass.card)
        .border_1()
        .border_color(edge)
        .shadow(vec![BoxShadow {
            color: glass.shadow,
            offset: point(px(0.), px(4.)),
            blur_radius: px(16.),
            spread_radius: px(-8.),
            inset: false,
        }])
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
            desk.drop_paths(&drop_id, paths.paths());
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
        .when(dropping, |card| card.child(drop_hint(&colors)))
        .children(find_bar)
        .into_any_element()
}

/// The floating capsule a pane shows at its top edge: what runs there, then
/// split, zoom and close.
fn pane_controls(
    desk: &Desk,
    placement: &Placement,
    zoomed: bool,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let title = desk.pane_title(placement.terminal_id.as_ref());
    let pane = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.link.as_ref().and_then(|link| link.connection.pane(id)));
    let cwd = desk::short_path(pane.and_then(|pane| pane.cwd.clone()));
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
        .rounded_full()
        .bg(from_hex(&colors.raised).opacity(0.9))
        .border_1()
        .border_color(from_hex(&colors.border))
        .shadow(vec![BoxShadow {
            color: gpui::black().opacity(0.22),
            offset: point(px(0.), px(4.)),
            blur_radius: px(14.),
            spread_radius: px(-2.),
            inset: false,
        }])
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
        .when(!cwd.is_empty(), |row| {
            row.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(from_hex(&colors.faint))
                    .child(cwd),
            )
        })
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
            "split-right",
            IconName::PanelRight,
            "Split Right",
            on_pane(id.clone(), "split-right"),
            cx,
        ))
        .child(header_button(
            &id,
            "split-down",
            IconName::PanelBottom,
            "Split Down",
            on_pane(id.clone(), "split-down"),
            cx,
        ))
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
        .child(header_button(
            &id,
            "close",
            IconName::Close,
            "Close Pane",
            on_pane(id.clone(), "close"),
            cx,
        ))
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
fn drop_hint(colors: &Palette) -> impl IntoElement {
    let accent = from_hex(&colors.theme.accent);
    div()
        .absolute()
        .inset_0()
        .rounded(px(RADIUS))
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
                .rounded(px(12.))
                .bg(from_hex(&colors.raised).opacity(0.92))
                .border_1()
                .border_color(from_hex(&colors.border))
                .shadow_lg()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(from_hex(&colors.theme.foreground))
                        .child("Drop to attach"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(from_hex(&colors.muted))
                        .child("Paths paste here, ready for your agent"),
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
        .tooltip(tooltip.to_owned())
        .on_click(cx.listener(move |desk, _, window, cx| {
            cx.stop_propagation();
            run(desk, window, cx);
        }))
}

/// Exceptions only: an unhealthy connection, agents that need a person, a
/// terminal still starting, and a pane scrolled away from live output.
fn status_bar(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
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
        .h(px(STATUS))
        .w_full()
        .flex_shrink_0()
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
                    .rounded(px(4.))
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
                    .rounded(px(4.))
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
                    .rounded(px(4.))
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
        Kind::Info | Kind::Refusal => colors.theme.accent.clone(),
    }
}

/// The newest agent or error entry, briefly, over the workspace. Everything
/// else waits behind the bell.
fn peek_card(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> Option<impl IntoElement> {
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
            .rounded(px(12.))
            .bg(from_hex(&colors.raised).opacity(0.94))
            .border_1()
            .border_l_4()
            .border_color(from_hex(&notification_edge(entry, colors)))
            .shadow_lg()
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

fn pill(text: &str, background: &str, color: &str) -> gpui::Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .rounded(px(8.))
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
    use super::ago_changed;

    #[test]
    fn ages_move_on_the_bucket_the_sidebar_shows() {
        assert!(!ago_changed(0, 3_000, 4_000));
        assert!(ago_changed(0, 4_000, 5_000));
        assert!(ago_changed(0, 10_000, 11_000));
        assert!(!ago_changed(0, 120_000, 121_000));
    }
}
