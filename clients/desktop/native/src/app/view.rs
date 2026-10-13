//! Terminal-first chrome: titlebar, sidebar, split tree, status, overlays.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, CursorStyle, DispatchPhase, Hsla, IntoElement, Keystroke,
    MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, SharedString, Styled, Window, canvas,
    div, px, relative,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{IconName, Sizable};
use phux_client_runtime::control::{PaneDescriptor, Status};

use crate::connection::AgentInfo;
use crate::settings::theme::Palette;
use crate::workspace::layout::{self, Axis, DeskTab, LayoutNode, Placement};
use crate::workspace::place::{self, SessionLine};

use super::actions::{OpenPathDirectory, Run};
use super::color::from_hex;
use super::desk::{self, Desk, Modal, ToastKind, Tone};
use super::keys::gpui_stroke;
use super::overlays;

const TITLEBAR: f32 = 40.;
const STATUS: f32 = 28.;
const DIVIDER: f32 = 5.;
/// Left inset that clears the traffic lights.
const TRAFFIC_LIGHTS: f32 = 78.;

pub fn render(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    let _frame = crate::diag::FRAME.timer();
    let colors = desk.palette();
    let sidebar = desk.prefs.sidebar_visible && desk.role != desk::WindowRole::Quick;
    desk.set_viewport(window.viewport_size());
    desk.sync_title(window);
    desk.settle_focus(window, cx);
    desk.ensure_clock(cx);
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

    div()
        .id("desk")
        .key_context("Desk")
        .track_focus(&desk.focus)
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(from_hex(&crate::settings::theme::with_alpha(
            &colors.theme.background,
            desk.prefs.terminal_opacity,
        )))
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
        .child(titlebar(desk, sidebar, &colors, cx))
        .child(
            div()
                .flex_grow(1.)
                .flex()
                .min_h(px(0.))
                .when(sidebar, |row| row.child(sidebar_column(desk, &colors, cx)))
                .child(main_column(desk, &colors, window, cx)),
        )
        .child(overlays::layer(desk, &colors, window, cx))
        .children(cursor.map(|cursor| drag_capture(cursor, cx)))
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
    let lead = if sidebar {
        (desk.prefs.sidebar_width as f32).max(TRAFFIC_LIGHTS + 36.)
    } else {
        TRAFFIC_LIGHTS + 36.
    };
    let centers = desk.tab_centers.clone();
    let quick = desk.role == desk::WindowRole::Quick;
    div()
        .h(px(TITLEBAR))
        .w_full()
        .relative()
        .flex()
        .items_center()
        .flex_shrink_0()
        .bg(from_hex(&colors.theme.surface))
        .border_b_1()
        .border_color(from_hex(&colors.border))
        .child(div().absolute().inset_0().child(region))
        .child(
            div()
                .w(px(lead))
                .h_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_end()
                .pl(px(TRAFFIC_LIGHTS))
                .pr(px(8.))
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
                }),
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
                .child(icon_button(
                    "split-right",
                    IconName::PanelRight,
                    "Split Right",
                    desk.shown_chord("split-right"),
                    cx.listener(|desk, _, window, cx| desk.execute("split-right", window, cx)),
                ))
                .child(icon_button(
                    "split-down",
                    IconName::PanelBottom,
                    "Split Down",
                    desk.shown_chord("split-down"),
                    cx.listener(|desk, _, window, cx| desk.execute("split-down", window, cx)),
                ))
                .child(icon_button(
                    "palette",
                    IconName::Search,
                    "Command Palette",
                    desk.shown_chord("palette"),
                    cx.listener(|desk, _, window, cx| desk.execute("palette", window, cx)),
                )),
        )
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
    let panes = layout::placements(&tab.root).len();
    let count = match (tab.zoomed_id.is_some(), panes) {
        (true, panes) => Some(format!("⤢ {panes}")),
        (false, 2..) => Some(panes.to_string()),
        _ => None,
    };
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
        .rounded(px(7.))
        .border_1()
        .border_color(if selected {
            from_hex(&colors.border)
        } else {
            gpui::transparent_black()
        })
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
                .bg(from_hex(agent_swatch(&state)))
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
                .rounded(px(4.))
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
        .bg(from_hex(&colors.theme.surface))
        .border_r_1()
        .border_color(from_hex(&colors.border))
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
        .filter(|pane| agents.contains_key(&pane.terminal_id))
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0);
    let sessions = session_groups(desk, colors, cx);
    let session_count = link
        .and_then(|link| link.connection.topology.as_ref())
        .map(|topology| topology.sessions.len())
        .unwrap_or(0);
    div()
        .id("sidebar-list")
        .flex_grow(1.)
        .overflow_y_scroll()
        .px(px(8.))
        .pb(px(8.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .when(!agent_panes.is_empty(), |list| {
            list.child(section_label("Agents", agent_panes.len(), colors))
                .children(agent_panes.iter().map(|pane| {
                    agent_row(desk, pane, agents.get(&pane.terminal_id), now, colors, cx)
                        .into_any_element()
                }))
                .child(div().h(px(8.)))
        })
        .child(section_label("Sessions", session_count, colors))
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

/// Sessions, home first, grouped by project tag and remote host when either exists.
fn session_groups(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> Vec<AnyElement> {
    let Some(link) = desk.link.as_ref() else {
        return Vec::new();
    };
    let Some(topology) = link.connection.topology.clone() else {
        return Vec::new();
    };
    let mut sessions = topology.sessions.clone();
    let home = desk.session.clone();
    sessions.sort_by(|left, right| {
        (left.name != home)
            .cmp(&(right.name != home))
            .then_with(|| left.name.cmp(&right.name))
    });
    let panes = link.connection.panes();
    let host_of = |session: u32| {
        let mut hosts: Vec<String> = panes
            .iter()
            .filter(|pane| pane.session_id == session)
            .filter_map(|pane| place::terminal_host(Some(&pane.terminal_id.to_string())))
            .collect();
        hosts.dedup();
        match hosts.len() {
            0 => None,
            1 => hosts.pop(),
            _ => Some("mixed remote".to_owned()),
        }
    };
    let lines: Vec<SessionLine> = sessions
        .iter()
        .map(|session| SessionLine {
            name: session.name.clone(),
            host: host_of(session.id),
            project: link.connection.projects.get(&session.id).cloned(),
        })
        .collect();
    let grouped =
        !link.connection.projects.is_empty() || lines.iter().any(|line| line.host.is_some());
    let mut rows = Vec::new();
    for group in place::group_by_project(&lines) {
        if grouped {
            let title = match (&group.project, &group.host) {
                (Some(project), Some(host)) => format!("{project} · {host}"),
                (Some(project), None) => project.clone(),
                (None, Some(host)) => host.clone(),
                (None, None) => "Local sessions".to_owned(),
            };
            rows.push(section_label(&title, group.sessions.len(), colors).into_any_element());
        }
        for line in group.sessions {
            if let Some(session) = sessions.iter().find(|session| session.name == line.name) {
                rows.push(session_block(desk, session.id, &session.name, colors, cx));
            }
        }
    }
    rows
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
    let collapsed = desk.collapsed.contains(name);
    let home = name == desk.session;
    let handle = cx.weak_entity();
    let menu = move |name: String| {
        let handle = handle.clone();
        move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let rename = handle.clone();
            let close = handle.clone();
            let (rename_name, close_name) = (name.clone(), name.clone());
            menu.item(
                PopupMenuItem::new("Rename Session…").on_click(move |_, window, cx| {
                    let current = rename_name.clone();
                    let _ = rename.update(cx, |desk, cx| {
                        desk.open_modal(
                            Modal::Session {
                                create: false,
                                current,
                            },
                            window,
                            cx,
                        )
                    });
                }),
            )
            .item(
                PopupMenuItem::new("Close Session…").on_click(move |_, window, cx| {
                    let name = close_name.clone();
                    let _ = close.update(cx, |desk, cx| {
                        desk.open_modal(Modal::CloseSession { name }, window, cx)
                    });
                }),
            )
        }
    };
    let toggle_name = name.to_owned();
    let header = div()
        .id(SharedString::from(format!("session-{id}")))
        .h(px(28.))
        .px(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|style| style.bg(from_hex(&colors.hover)))
        .child(
            div()
                .w(px(10.))
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
        .when(home, |row| {
            row.child(pill("home", &colors.accent_wash, &colors.theme.accent))
        })
        .child(
            div()
                .text_xs()
                .text_color(from_hex(&colors.faint))
                .child(panes.len().to_string()),
        )
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
        .context_menu(menu(name.to_owned()));
    div()
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(header)
        .when(!collapsed, |block| {
            block.children(
                panes
                    .iter()
                    .map(|pane| pane_row(desk, pane, colors, cx).into_any_element()),
            )
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
    let focused = desk.focused_terminal().as_ref() == Some(&terminal);
    let visible = desk.active_tab().is_some_and(|tab| {
        layout::placements(&tab.root)
            .iter()
            .any(|placement| placement.terminal_id.as_ref() == Some(&terminal))
    });
    let views = desk
        .link
        .as_ref()
        .map(|link| link.workspace.views_of(&terminal))
        .unwrap_or(0);
    let agent = desk.agent(&terminal).cloned();
    let subtitle = Some(desk::short_path(pane.cwd.clone()))
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| terminal.to_string());
    let glyph = match agent.as_ref() {
        Some(info) => div()
            .size(px(8.))
            .rounded_full()
            .bg(from_hex(agent_swatch(&info.state)))
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
    div()
        .id(SharedString::from(format!("pane-{terminal}")))
        .min_h(px(38.))
        .pl(px(10.))
        .pr(px(8.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(9.))
        .rounded(px(6.))
        .cursor_pointer()
        .bg(if focused {
            from_hex(&colors.active)
        } else {
            gpui::transparent_black()
        })
        .hover(|style| style.bg(from_hex(&colors.hover)))
        .child(
            div()
                .w(px(14.))
                .flex()
                .justify_center()
                .flex_shrink_0()
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
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
                        .child(desk::pane_label(Some(pane))),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(from_hex(&colors.muted))
                        .child(subtitle),
                ),
        )
        .when(views > 1, |row| {
            row.child(pill(
                &format!("{views} views"),
                &colors.hover,
                &colors.muted,
            ))
        })
        .on_click(cx.listener(move |desk, _, window, cx| {
            desk.reveal_terminal(&terminal);
            desk.after_command(window, cx);
        }))
}

fn agent_row(
    desk: &Desk,
    pane: &PaneDescriptor,
    agent: Option<&AgentInfo>,
    now: u64,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let terminal = pane.terminal_id.clone();
    let focused = desk.focused_terminal().as_ref() == Some(&terminal);
    let state = agent
        .map(|info| info.state.clone())
        .unwrap_or_else(|| "unknown".into());
    let tint = agent_swatch(&state);
    let urgent = agent.is_some_and(|info| info.attention == "high");
    let changed = agent.map(|info| info.changed_at_ms).unwrap_or(now);
    div()
        .id(SharedString::from(format!("agent-{terminal}")))
        .min_h(px(40.))
        .pl(px(10.))
        .pr(px(8.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(9.))
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
                .w(px(14.))
                .flex()
                .justify_center()
                .child(div().size(px(8.)).rounded_full().bg(from_hex(tint))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(
                            agent
                                .map(|info| info.name.clone())
                                .unwrap_or_else(|| "agent".into()),
                        ),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(from_hex(&colors.muted))
                        .child(desk::pane_label(Some(pane))),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_end()
                .flex_shrink_0()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(from_hex(tint))
                        .child(state),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(from_hex(&colors.faint))
                        .child(ago(changed, now)),
                ),
        )
        .on_click(cx.listener(move |desk, _, window, cx| {
            desk.reveal_terminal(&terminal);
            desk.after_command(window, cx);
        }))
}

/// Seconds, minutes, hours or days since `then`, both in Unix milliseconds.
fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then) / 1000;
    match seconds {
        0..5 => "now".into(),
        5..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", (seconds + 30) / 60),
        3600..86400 => format!("{}h", (seconds + 1800) / 3600),
        _ => format!("{}d", (seconds + 43200) / 86400),
    }
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
    let split = root
        .as_ref()
        .is_some_and(|root| matches!(root, LayoutNode::Split { .. }))
        && zoomed.is_none();
    div()
        .flex_grow(1.)
        .min_h(px(0.))
        .relative()
        .when(split, |body| body.p(px(4.)))
        .child(if let Some(id) = zoomed {
            zoomed_pane(desk, &id, window, cx).into_any_element()
        } else if let Some(root) = root {
            split_tree(desk, &root, colors, window, cx).into_any_element()
        } else {
            empty_state(desk, colors, cx).into_any_element()
        })
        .child(toast_stack(desk, colors, cx))
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
            .child(format!("Connecting to {}…", desk.socket.display())),
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
    desk.placement(id)
        .cloned()
        .map(|placement| pane_view(desk, &placement, window, cx))
        .unwrap_or_else(|| div().into_any_element())
}

fn split_tree(
    desk: &mut Desk,
    node: &LayoutNode,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    match node {
        LayoutNode::Leaf { placement } => pane_view(desk, placement, window, cx).into_any_element(),
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
            div()
                .size_full()
                .flex()
                .when(horizontal, |row| row.flex_row())
                .when(!horizontal, |column| column.flex_col())
                .child(
                    div()
                        .when(horizontal, |side| side.w(relative(*ratio)).h_full())
                        .when(!horizontal, |side| side.h(relative(*ratio)).w_full())
                        .flex_shrink_0()
                        .child(split_tree(desk, first, colors, window, cx)),
                )
                .child(split_divider(desk, id.clone(), *axis, colors, cx))
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

/// A 5 px grab area with a 1 px hairline in the theme's divider colour, accent while dragged.
fn split_divider(
    desk: &Desk,
    id: String,
    axis: Axis,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let horizontal = matches!(axis, Axis::Row);
    let active = desk.dragging_split() == Some(id.as_str());
    let line = if active {
        from_hex(&colors.theme.accent)
    } else {
        from_hex(&colors.divider)
    };
    let hover = from_hex(&colors.theme.accent).opacity(0.6);
    div()
        .id(SharedString::from(format!("split-{id}")))
        .group("divider")
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .when(horizontal, |bar| {
            bar.w(px(DIVIDER)).h_full().cursor_col_resize()
        })
        .when(!horizontal, |bar| {
            bar.h(px(DIVIDER)).w_full().cursor_row_resize()
        })
        .child(
            div()
                .bg(line)
                .group_hover("divider", move |style| style.bg(hover))
                .when(horizontal, |hairline| hairline.w(px(1.)).h_full())
                .when(!horizontal, |hairline| hairline.h(px(1.)).w_full()),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |desk, _, _, cx| {
                desk.begin_split_drag(id.clone(), axis, cx);
                cx.stop_propagation();
            }),
        )
}

fn pane_view(
    desk: &mut Desk,
    placement: &Placement,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let focused =
        desk.focused_placement().map(|item| item.id.as_str()) == Some(placement.id.as_str());
    let colors = desk.palette();
    let placement_id = placement.id.clone();
    let terminal = placement.terminal_id.clone();
    let view = placement.view_id;
    // The terminal holds the keyboard only while no modal or find field does.
    let settings = desk.terminal_settings(placement, focused && !desk.text_field_has_keys());
    let client = desk.client();
    let body = if let (Some(terminal), Some(view), Some(settings), Some(client)) =
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
        div().size_full().child(entity).into_any_element()
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
    let chrome = leaves > 1 || zoomed;
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
    let header = chrome.then(|| placement_header(desk, placement, focused, zoomed, &colors, cx));
    div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .when(chrome, |pane| {
            pane.rounded(px(6.))
                .border_1()
                .border_color(from_hex(if focused {
                    &colors.theme.accent
                } else {
                    &colors.border
                }))
        })
        .block_mouse_except_scroll()
        .children(header)
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
                )
                .children(find_bar),
        )
        .into_any_element()
}

fn placement_header(
    desk: &Desk,
    placement: &Placement,
    focused: bool,
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
    let views = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.link.as_ref().map(|link| link.workspace.views_of(id)))
        .unwrap_or(0);
    let fenced = placement.terminal_id.as_ref().is_some_and(|id| {
        desk.link
            .as_ref()
            .is_some_and(|link| link.connection.fenced(id))
    });
    let id = placement.id.clone();
    let focus_id = placement.id.clone();
    div()
        .id(SharedString::from(format!("header-{id}")))
        .h(px(28.))
        .pl(px(10.))
        .pr(px(4.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(8.))
        .bg(from_hex(if focused {
            &colors.raised
        } else {
            &colors.theme.surface
        }))
        .border_b_1()
        .border_color(from_hex(&colors.border))
        .child(
            div()
                .flex_shrink_0()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(from_hex(if focused {
                    &colors.theme.foreground
                } else {
                    &colors.muted
                }))
                .child(title),
        )
        .when(!cwd.is_empty(), |row| {
            row.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(from_hex(&colors.faint))
                    .child(cwd),
            )
        })
        .children(agent.map(|info| {
            pill(
                &format!("{} · {}", info.name, info.state),
                &format!("{}22", agent_swatch(&info.state)),
                agent_swatch(&info.state),
            )
        }))
        .when(views > 1, |row| {
            row.child(pill(
                &format!("{views} views"),
                &colors.hover,
                &colors.muted,
            ))
        })
        .when(fenced, |row| {
            row.child(pill(
                "⚡ delivery unknown",
                &format!("{}22", colors.theme.warning),
                &colors.theme.warning,
            ))
        })
        .child(div().flex_1())
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

fn status_bar(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let (label, tone) = desk.connection_label();
    let unhealthy = desk.unhealthy();
    let error = unhealthy
        .then(|| {
            desk.connect_error.clone().or_else(|| {
                desk.link
                    .as_ref()
                    .and_then(|link| link.connection.error.clone())
            })
        })
        .flatten();
    let frame = desk
        .focused_view()
        .and_then(|view| desk.client().and_then(|client| client.acquire_view(view)));
    let geometry = frame
        .as_ref()
        .map(|frame| format!("{}×{}", frame.cols, frame.rows));
    let scrolled = frame
        .as_ref()
        .is_some_and(|frame| !frame.scrollbar.at_tail());
    let fenced = desk.focused_terminal().is_some_and(|id| {
        desk.link
            .as_ref()
            .is_some_and(|link| link.connection.fenced(&id))
    });
    let pending = desk
        .link
        .as_ref()
        .map(|link| link.workspace.pending())
        .unwrap_or(0);
    let mut counts: Vec<(String, usize)> = Vec::new();
    if let Some(link) = desk.link.as_ref() {
        for info in link.connection.agents.values() {
            match counts.iter_mut().find(|(state, _)| *state == info.state) {
                Some((_, count)) => *count += 1,
                None => counts.push((info.state.clone(), 1)),
            }
        }
    }
    counts.sort_by_key(|(state, _)| agent_rank_state(state));
    let tone_color = from_hex(match tone {
        Tone::Good => &colors.theme.success,
        Tone::Busy => &colors.theme.warning,
        Tone::Bad => &colors.theme.danger,
    });
    let theme_name = colors.theme.name.clone();
    let size = desk.prefs.font_size;
    let hover = from_hex(&colors.hover);
    div()
        .h(px(STATUS))
        .w_full()
        .flex_shrink_0()
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .bg(from_hex(&colors.theme.surface))
        .border_t_1()
        .border_color(from_hex(&colors.border))
        .text_xs()
        .text_color(from_hex(&colors.muted))
        .child(
            div()
                .id("status-connection")
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(4.))
                .rounded(px(4.))
                .child(div().size(px(7.)).rounded_full().bg(tone_color))
                .child(
                    div()
                        .text_color(if unhealthy {
                            tone_color
                        } else {
                            from_hex(&colors.subtext)
                        })
                        .child(label),
                )
                .when(unhealthy, |status| {
                    status
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(
                            cx.listener(|desk, _, window, cx| {
                                desk.execute("reconnect", window, cx)
                            }),
                        )
                }),
        )
        .children(error.map(|error| {
            div()
                .min_w_0()
                .truncate()
                .text_color(from_hex(&colors.theme.danger))
                .child(error)
        }))
        .when(pending > 0, |bar| bar.child("starting terminal…"))
        .when(fenced, |bar| {
            bar.child(
                div()
                    .text_color(from_hex(&colors.theme.warning))
                    .child("⚡ input waiting for presented output"),
            )
        })
        .children(counts.into_iter().map(|(state, count)| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(from_hex(agent_swatch(&state))),
                )
                .child(format!("{count} {state}"))
        }))
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
        .children(geometry.map(|geometry| div().font_family("Menlo").child(geometry)))
        .child(
            div()
                .id("status-settings")
                .px(px(4.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(format!("{theme_name} · {size}pt"))
                .on_click(cx.listener(|desk, _, window, cx| desk.execute("settings", window, cx))),
        )
}

fn toast_stack(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .absolute()
        .bottom(px(12.))
        .right(px(12.))
        .w(px(340.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .children(desk.toasts.iter().map(|toast| {
            let id = toast.id;
            let terminal = toast.terminal.clone();
            let edge = match toast.kind {
                ToastKind::Error => colors.theme.danger.clone(),
                ToastKind::Success => colors.theme.success.clone(),
                ToastKind::Agent => toast
                    .agent_state
                    .as_deref()
                    .map(agent_swatch)
                    .map(str::to_owned)
                    .unwrap_or_else(|| colors.theme.warning.clone()),
                ToastKind::Info => colors.theme.accent.clone(),
            };
            div()
                .id(("toast", id as usize))
                .p(px(10.))
                .pl(px(12.))
                .rounded(px(8.))
                .bg(from_hex(&colors.raised))
                .border_1()
                .border_l_4()
                .border_color(from_hex(&edge))
                .shadow_lg()
                .occlude()
                .when(terminal.is_some(), |card| card.cursor_pointer())
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
                                .child(toast.title.clone()),
                        )
                        .children(toast.body.clone().map(|body| {
                            div()
                                .text_xs()
                                .line_clamp(3)
                                .text_color(from_hex(&colors.muted))
                                .child(body)
                        })),
                )
                .child(
                    Button::new(("toast-dismiss", id as usize))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tooltip("Dismiss")
                        .on_click(cx.listener(move |desk, _, _, cx| {
                            cx.stop_propagation();
                            desk.dismiss_toast(id);
                            cx.notify();
                        })),
                )
                .on_click(cx.listener(move |desk, _, window, cx| {
                    if let Some(terminal) = terminal.clone() {
                        desk.reveal_terminal(&terminal);
                        desk.after_command(window, cx);
                    }
                    desk.dismiss_toast(id);
                    cx.notify();
                }))
        }))
}

fn section_label(title: &str, count: usize, colors: &Palette) -> gpui::Div {
    div()
        .px(px(8.))
        .pt(px(8.))
        .h(px(28.))
        .flex()
        .items_center()
        .text_xs()
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(from_hex(&colors.faint))
        .child(div().flex_1().child(title.to_uppercase()))
        .child(count.to_string())
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

fn agent_rank(agent: Option<&AgentInfo>) -> u8 {
    agent_rank_state(agent.map(|info| info.state.as_str()).unwrap_or("unknown"))
}

fn agent_rank_state(state: &str) -> u8 {
    match state {
        "blocked" => 0,
        "done" => 1,
        "working" => 2,
        "idle" => 3,
        _ => 4,
    }
}

fn strongest_agent(desk: &Desk, tab: &DeskTab) -> Option<String> {
    let order = ["blocked", "done", "working", "idle", "unknown"];
    let states: Vec<_> = layout::placements(&tab.root)
        .into_iter()
        .filter_map(|placement| {
            placement
                .terminal_id
                .as_ref()
                .and_then(|id| desk.agent(id))
                .map(|info| info.state.clone())
        })
        .collect();
    order
        .iter()
        .find(|state| states.iter().any(|item| item == *state))
        .map(|state| (*state).to_owned())
}

fn agent_swatch(state: &str) -> &'static str {
    for (name, color) in crate::settings::theme::agent_colors() {
        if name == state {
            return color;
        }
    }
    "#7f849c"
}
