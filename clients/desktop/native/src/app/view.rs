//! Terminal-first chrome: titlebar, sidebar, split tree, status, overlays.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, IntoElement, ParentElement, Pixels, Render, Styled, Window, div, point, px, relative, size,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::*;
use phux_protocol::ResourceId;

use crate::connection::AgentInfo;
use crate::settings::theme::{self, Palette};
use crate::terminal::TerminalEvent;
use crate::workspace::layout::{self, Axis, DeskTab, LayoutNode, Placement};

use super::color::from_hex;
use super::desk::{self, Desk, Modal, ToastKind};
use super::overlays;

const TITLEBAR: f32 = 40.;
const STATUS: f32 = 28.;
const DIVIDER: f32 = 5.;

pub fn render(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    let colors = desk.palette();
    let sidebar = desk.prefs.sidebar_visible && desk.role != desk::WindowRole::Quick;
    let sidebar_width = if sidebar { desk.prefs.sidebar_width as f32 } else { 0. };
    let viewport = window.viewport_size();
    desk.set_viewport(viewport);
    let origin = point(px(sidebar_width), px(TITLEBAR));
    let content = size(
        (viewport.width - px(sidebar_width)).max(px(1.)),
        (viewport.height - px(TITLEBAR + STATUS)).max(px(1.)),
    );
    desk.set_content_bounds(origin, content);
    let title = desk.active_tab().map(|tab| desk::tab_title(desk, tab)).unwrap_or_else(|| "phux".into());
    window.set_window_title(&format!("{title} — phux"));
    if crate::diag::evidence_pending(cx) {
        let desk_handle = cx.weak_entity();
        crate::diag::arm_evidence(
            Box::new(move |app| desk_handle.upgrade().map(|desk| desk.read(app).frame_report(app))),
            window,
            cx,
        );
    }

    div()
        .id("desk")
        .key_context("Desk")
        .track_focus(&desk.focus)
        .size_full()
        .flex()
        .flex_col()
        .bg(from_hex(&crate::settings::theme::with_alpha(&colors.theme.background, desk.prefs.terminal_opacity)))
        .on_action(cx.listener(|desk, action: &super::actions::Run, window, cx| {
            desk.execute(&action.id, window, cx);
        }))
        .on_action(cx.listener(|desk, _: &super::actions::Quit, _window, cx| {
            desk.desktop.update(cx, |desktop, cx| desktop.quit(cx));
        }))
        .on_key_down(cx.listener(|desk, event: &gpui::KeyDownEvent, window, cx| {
            desk.on_key(event, window, cx);
        }))
        .on_mouse_move(cx.listener(|desk, event: &gpui::MouseMoveEvent, _window, cx| {
            if event.pressed_button.is_some() {
                desk.drag_move(event.position.x.as_f32(), event.position.y.as_f32(), cx);
            }
        }))
        .on_mouse_up(gpui::MouseButton::Left, cx.listener(|desk, _: &gpui::MouseUpEvent, _, cx| {
            desk.end_drag();
            cx.notify();
        }))
        .child(titlebar(desk, sidebar, &colors, cx))
        .child(
            div().flex_grow(1.).flex().min_h(px(0.)).child(if sidebar {
                sidebar_column(desk, &colors, cx).into_any_element()
            } else {
                div().into_any_element()
            })
            .child(main_column(desk, &colors, window, cx)),
        )
        .child(overlays::layer(desk, &colors, window, cx))
}

fn titlebar(desk: &Desk, sidebar: bool, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let tabs = desk.link.as_ref().map(|link| link.workspace.tabs.clone()).unwrap_or_default();
    let active = desk.link.as_ref().map(|link| link.workspace.active_id.clone()).unwrap_or_default();
    let mut buttons = Vec::new();
    for tab in &tabs {
        buttons.push(tab_button(desk, tab, &active, colors, cx).into_any_element());
    }
    div()
        .h(px(TITLEBAR))
        .w_full()
        .flex()
        .items_center()
        .pl(px(78.))
        .pr(px(8.))
        .gap(px(4.))
        .child(drag_region(cx))
        .children(buttons)
        .child(icon_button("new-tab", "New Terminal", "+", cx.listener(|desk, _, window, cx| {
            desk.execute("new", window, cx);
        })))
        .child(icon_button("sidebar", "Toggle Sidebar", if sidebar { "Sidebar" } else { "Show" }, cx.listener(|desk, _, window, cx| {
            desk.execute("sidebar", window, cx);
        })))
        .child(icon_button("split-right", "Split Right", "Split", cx.listener(|desk, _, window, cx| {
            desk.execute("split-right", window, cx);
        })))
        .child(icon_button("palette", "Command Palette", "⌘⇧P", cx.listener(|desk, _, window, cx| {
            desk.execute("palette", window, cx);
        })))
}

fn drag_region(cx: &mut Context<Desk>) -> gpui::AnyElement {
    let region = cx.new(|cx| crate::chrome::DragRegion::new(cx));
    div().flex_grow(1.).h_full().child(region).into_any_element()
}

fn tab_button(desk: &Desk, tab: &DeskTab, active: &str, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let id = tab.id.clone();
    let selected = tab.id == active;
    let title = desk::tab_title(desk, tab);
    let agent = strongest_agent(desk, tab);
    div()
        .id(gpui::ElementId::Name(id.clone().into()))
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .bg(if selected { from_hex(&colors.active) } else { gpui::transparent_black() })
        .child(div().text_sm().text_color(from_hex(&colors.theme.foreground)).child(title))
        .children(agent.map(|state| {
            div().size(px(7.)).rounded_full().bg(from_hex(agent_swatch(&state)))
        }))
        .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |desk, event: &gpui::MouseDownEvent, _, cx| {
            desk.select_tab(&id);
            desk.begin_tab_drag(id.clone(), f32::from(event.position.x));
            cx.notify();
        }))
        .on_click(cx.listener({
            let id = tab.id.clone();
            move |desk, event: &ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    desk.modal = Modal::Rename { tab_id: id.clone() };
                    cx.notify();
                }
            }
        }))
}

fn sidebar_column(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .w(px(desk.prefs.sidebar_width as f32))
        .h_full()
        .flex()
        .flex_col()
        .bg(from_hex(&colors.theme.surface))
        .child(sidebar_list(desk, colors, cx))
        .child(sidebar_resize(colors, cx))
}

fn sidebar_list(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let link = desk.link.as_ref();
    let agents = link.map(|link| link.connection.agents.clone()).unwrap_or_default();
    let panes = link.map(|link| link.connection.panes().to_vec()).unwrap_or_default();
    let mut agent_panes: Vec<_> = panes.iter().filter(|pane| agents.contains_key(&pane.terminal_id)).cloned().collect();
    agent_panes.sort_by_key(|pane| agent_rank(agents.get(&pane.terminal_id)));
    div()
        .id("sidebar-list")
        .flex_grow(1.)
        .overflow_y_scroll()
        .p(px(8.))
        .child(section_label("Agents", agent_panes.len(), colors))
        .children(agent_panes.into_iter().map(|pane| {
            let info = agents.get(&pane.terminal_id).cloned();
            pane_row(desk, pane.terminal_id.clone(), info, pane_label(&pane), colors, cx).into_any_element()
        }))
        .child(section_label("Sessions", link.map(|link| link.connection.topology.as_ref().map(|topology| topology.sessions.len()).unwrap_or(0)).unwrap_or(0), colors))
        .children(session_rows(desk, colors, cx))
}

fn session_rows(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> Vec<AnyElement> {
    let Some(link) = desk.link.as_ref() else { return Vec::new() };
    let Some(topology) = link.connection.topology.clone() else { return Vec::new() };
    let mut sessions = topology.sessions.clone();
    let home = desk.session.clone();
    sessions.sort_by(|left, right| {
        let rank = |name: &str| if name == home { 0 } else { 1 };
        rank(&left.name).cmp(&rank(&right.name)).then_with(|| left.name.cmp(&right.name))
    });
    sessions.into_iter().map(|session| {
        let panes: Vec<_> = link.connection.panes().iter().filter(|pane| pane.session_id == session.id).cloned().collect();
        let id = session.id;
        let name = session.name.clone();
        div()
            .child(
                div().h(px(28.)).flex().items_center().justify_between().px(px(6.)).child(
                    div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(from_hex(&colors.theme.foreground)).child(name.clone()),
                )
                .child(icon_button(&format!("session-new-{id}"), "New terminal in session", "+", cx.listener(move |desk, _, _, cx| {
                    desk.new_terminal_in(id);
                    cx.notify();
                })))
                .child(icon_button(&format!("session-menu-{id}"), "Session actions", "…", cx.listener(move |desk, _, window, cx| {
                    open_session_menu(desk, name.clone(), window, cx);
                }))),
            )
            .children(panes.into_iter().map(|pane| {
                pane_row(desk, pane.terminal_id.clone(), desk.agent(&pane.terminal_id).cloned(), pane_label(&pane), colors, cx).into_any_element()
            }))
            .into_any_element()
    }).collect()
}

fn open_session_menu(desk: &mut Desk, name: String, window: &mut Window, cx: &mut Context<Desk>) {
    desk.modal = Modal::Session { create: false, current: name.clone() };
    let _ = window;
    cx.notify();
}

fn pane_row(desk: &Desk, terminal: ResourceId, agent: Option<AgentInfo>, title: String, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let terminal = terminal.clone();
    let focused = desk.focused_placement().and_then(|placement| placement.terminal_id.as_ref()) == Some(&terminal);
    div()
        .id(gpui::ElementId::Name(format!("pane-{}", terminal).into()))
        .h(px(28.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(6.))
        .bg(if focused { from_hex(&colors.accent_wash) } else { gpui::transparent_black() })
        .child(div().size(px(8.)).rounded_full().bg(from_hex(agent_swatch(agent.as_ref().map(|info| info.state.as_str()).unwrap_or("unknown")))))
        .child(div().text_sm().text_color(from_hex(&colors.theme.foreground)).child(title))
        .on_click(cx.listener(move |desk, _, _, cx| {
            desk.reveal_terminal(&terminal);
            cx.notify();
        }))
}

fn sidebar_resize(colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .id("sidebar-resize")
        .absolute()
        .right(px(0.))
        .top(px(0.))
        .w(px(4.))
        .h_full()
        .cursor_col_resize()
        .bg(from_hex(&colors.border))
        .on_mouse_down(gpui::MouseButton::Left, cx.listener(|desk, _, _, cx| {
            desk.begin_sidebar_drag();
            cx.notify();
        }))
}

fn main_column(desk: &mut Desk, colors: &Palette, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .flex_grow(1.)
        .min_w(px(0.))
        .flex()
        .flex_col()
        .child(workspace_body(desk, colors, window, cx))
        .child(status_bar(desk, colors, cx))
}

fn workspace_body(desk: &mut Desk, colors: &Palette, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    let zoomed = desk.active_tab().and_then(|tab| tab.zoomed_id.clone());
    let root = desk.active_tab().map(|tab| tab.root.clone());
    div()
        .flex_grow(1.)
        .min_h(px(0.))
        .relative()
        .child(if root.is_none() {
            empty_state(desk, colors, cx).into_any_element()
        } else if let Some(id) = zoomed {
            zoomed_pane(desk, &id, window, cx).into_any_element()
        } else if let Some(root) = root {
            split_tree(desk, &root, window, cx).into_any_element()
        } else {
            div().into_any_element()
        })
        .child(toast_stack(desk, colors, cx))
}

fn empty_state(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.link.as_ref().map(|link| format!("{:?}", link.connection.status)).unwrap_or_else(|| "Not connected".into());
    let error = desk.connect_error.clone().or_else(|| desk.link.as_ref().and_then(|link| link.connection.error.clone()));
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(div().text_color(from_hex(&colors.theme.foreground)).child("phux"))
        .child(div().text_sm().text_color(from_hex(&colors.muted)).child(status))
        .children(error.map(|error| div().text_sm().text_color(from_hex(&colors.theme.danger)).child(error)))
        .child(div().text_sm().text_color(from_hex(&colors.muted)).child(desk.socket.display().to_string()))
        .child(Button::new("empty-new").label("New Terminal").primary().on_click(cx.listener(|desk, _, window, cx| desk.execute("new", window, cx))))
        .child(Button::new("empty-folder").label("Open Folder").on_click(cx.listener(|desk, _, window, cx| desk.execute("folder", window, cx))))
        .child(Button::new("empty-reconnect").label("Reconnect").on_click(cx.listener(|desk, _, window, cx| desk.execute("reconnect", window, cx))))
}

fn zoomed_pane(desk: &mut Desk, id: &str, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    desk.placement(id).cloned().map(|placement| pane_view(desk, &placement, window, cx)).unwrap_or_else(|| div().into_any_element())
}

fn split_tree(desk: &mut Desk, node: &LayoutNode, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    match node {
        LayoutNode::Leaf { placement } => pane_view(desk, placement, window, cx).into_any_element(),
        LayoutNode::Split { id, axis, ratio, first, second } => {
            let id = id.clone();
            let horizontal = matches!(axis, Axis::Row);
            div()
                .size_full()
                .flex()
                .when(horizontal, |row| row.flex_row())
                .when(!horizontal, |column| column.flex_col())
                .child(div().when(horizontal, |side| side.w(relative(*ratio))).when(!horizontal, |side| side.h(relative(*ratio))).child(split_tree(desk, first, window, cx)))
                .child(split_divider(id, *axis, cx))
                .child(div().flex_grow(1.).child(split_tree(desk, second, window, cx)))
                .into_any_element()
        }
    }
}

fn split_divider(id: String, axis: Axis, cx: &mut Context<Desk>) -> impl IntoElement {
    let horizontal = matches!(axis, Axis::Row);
    div()
        .id(gpui::ElementId::Name(format!("split-{id}").into()))
        .when(horizontal, |bar| bar.w(px(DIVIDER)).h_full().cursor_col_resize())
        .when(!horizontal, |bar| bar.h(px(DIVIDER)).w_full().cursor_row_resize())
        .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |desk, _, _, cx| {
            desk.begin_split_drag(id.clone(), axis);
            cx.notify();
        }))
}

fn pane_view(desk: &mut Desk, placement: &Placement, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let focused = desk.focused_placement().map(|item| item.id.as_str()) == Some(placement.id.as_str());
    let colors = desk.palette();
    let placement_id = placement.id.clone();
    let terminal = placement.terminal_id.clone();
    let view = placement.view_id;
    let settings = desk.terminal_settings(placement, focused);
    let client = desk.client();
    let body = if let (Some(terminal), Some(view), Some(settings), Some(client)) = (terminal, view, settings, client) {
        let entity = desk.panes_mut().entity_for(&placement_id, &terminal, view, settings, &client, window, cx);
        subscribe_terminal(desk, &placement_id, &entity, window, cx);
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
    let find = desk.find.as_ref().filter(|find| find.placement_id == placement.id).cloned();
    let find_bar = find.map(|state| overlays::find_bar(desk, state.query, state.case_sensitive, &colors, window, cx));
    let header = placement_header(desk, placement, focused, &colors, cx);
    div()
        .size_full()
        .flex()
        .flex_col()
        .block_mouse_except_scroll()
        .when(!focused && desk.active_tab().is_some_and(|tab| layout::placements(&tab.root).len() > 1), |pane| {
            pane.opacity(desk.prefs.unfocused_opacity as f32)
        })
        .child(header)
        .child(div().flex_grow(1.).min_h(px(0.)).child(body))
        .children(find_bar)
        .into_any_element()
}

fn subscribe_terminal(desk: &mut Desk, placement_id: &str, entity: &gpui::Entity<crate::terminal::Terminal>, window: &mut Window, cx: &mut Context<Desk>) {
    let placement_id = placement_id.to_owned();
    if desk.subscribed_terminal(&placement_id) {
        return;
    }
    let remembered = placement_id.to_owned();
    let sub = cx.subscribe_in(entity, window, move |desk, _, event, window, cx| {
        match event {
            TerminalEvent::Activated => {
                if let Some(tab) = desk.active_tab_id_owned() {
                    desk.focus_placement(&tab, &remembered, window, cx);
                }
            }
            TerminalEvent::PathsDropped(paths) => desk.drop_paths(paths),
        }
        cx.notify();
    });
    desk.remember_terminal_sub(placement_id.to_owned(), sub);
}

fn placement_header(desk: &Desk, placement: &Placement, focused: bool, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let title = desk.pane_title(placement.terminal_id.as_ref());
    let agent = placement.terminal_id.as_ref().and_then(|id| desk.agent(id)).cloned();
    let id = placement.id.clone();
    div()
        .id(gpui::ElementId::Name(format!("header-{id}").into()))
        .h(px(28.))
        .px(px(8.))
        .flex()
        .items_center()
        .justify_between()
        .bg(from_hex(&colors.theme.surface))
        .border_b_1()
        .border_color(from_hex(&colors.border))
        .child(div().flex().items_center().gap(px(6.)).child(div().text_sm().text_color(from_hex(if focused { &colors.theme.foreground } else { &colors.muted })).child(title)).children(agent.map(|info| {
            div().text_xs().text_color(from_hex(agent_swatch(&info.state))).child(info.state)
        })))
        .child(div().flex().gap(px(4.))
            .child(icon_button(&format!("zoom-{id}"), "Zoom", "Zoom", cx.listener({
                let id = id.clone();
                move |desk, _, window, cx| {
                    if let Some(tab) = desk.active_tab_id_owned() {
                        desk.focus_placement(&tab, &id, window, cx);
                    }
                    desk.execute("zoom", window, cx);
                }
            })))
            .child(icon_button(&format!("close-{id}"), "Close pane", "Close", cx.listener(move |desk, _, _, cx| {
                desk.workspace_close(&id);
                cx.notify();
            }))))
        .on_click(cx.listener({
            let id = placement.id.clone();
            move |desk, event: &ClickEvent, window, cx| {
                if let Some(tab) = desk.active_tab_id_owned() {
                    desk.focus_placement(&tab, &id, window, cx);
                }
                if event.click_count() >= 2 {
                    desk.execute("zoom", window, cx);
                }
            }
        }))
}

fn status_bar(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.link.as_ref().map(|link| format!("{:?}", link.connection.status)).unwrap_or_else(|| "offline".into());
    let error = desk.link.as_ref().and_then(|link| link.connection.error.clone());
    let geometry = desk.focused_view().and_then(|view| desk.client().and_then(|client| client.acquire_view(view))).map(|frame| format!("{}×{}", frame.cols, frame.rows));
    let scrolled = desk.focused_view().and_then(|view| desk.client().and_then(|client| client.acquire_view(view))).is_some_and(|frame| !frame.scrollbar.at_tail());
    let fenced = desk.focused_terminal().is_some_and(|id| desk.link.as_ref().is_some_and(|link| link.connection.fenced(&id)));
    div()
        .h(px(STATUS))
        .w_full()
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .bg(from_hex(&colors.theme.surface))
        .border_t_1()
        .border_color(from_hex(&colors.border))
        .text_xs()
        .text_color(from_hex(&colors.muted))
        .child(status)
        .children(error.map(|error| div().text_color(from_hex(&colors.theme.danger)).child(error)))
        .child(desk.session.clone())
        .children(geometry)
        .when(scrolled, |bar| bar.child("scrolled back · ⌘L live"))
        .when(fenced, |bar| bar.child("input waiting for presented output"))
        .child(div().flex_grow(1.))
        .child(Button::new("status-live").label("Live").ghost().on_click(cx.listener(|desk, _, window, cx| desk.execute("follow", window, cx))))
        .child(Button::new("status-reconnect").label("Reconnect").ghost().on_click(cx.listener(|desk, _, window, cx| desk.execute("reconnect", window, cx))))
        .child(Button::new("status-settings").label("Settings").ghost().on_click(cx.listener(|desk, _, window, cx| desk.execute("settings", window, cx))))
}

fn toast_stack(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .absolute()
        .bottom(px(12.))
        .right(px(12.))
        .w(px(320.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .children(desk.toasts.iter().map(|toast| {
            let id = toast.id;
            let terminal = toast.terminal.clone();
            div()
                .p(px(10.))
                .rounded(px(8.))
                .bg(from_hex(&colors.raised))
                .border_1()
                .border_color(from_hex(match toast.kind {
                    ToastKind::Error => &colors.theme.danger,
                    ToastKind::Success => &colors.theme.success,
                    ToastKind::Agent => &colors.theme.warning,
                    ToastKind::Info => &colors.border,
                }))
                .child(div().text_sm().text_color(from_hex(&colors.theme.foreground)).child(toast.title.clone()))
                .children(toast.body.clone().map(|body| div().text_xs().text_color(from_hex(&colors.muted)).child(body)))
                .child(Button::new(("dismiss", id as usize)).label("Dismiss").ghost().on_click(cx.listener(move |desk, _, _, cx| {
                    desk.dismiss_toast(id);
                    cx.notify();
                })))
                .when(terminal.is_some(), |card| {
                    card.child(Button::new(("open-toast", id as usize)).label("Show").on_click(cx.listener(move |desk, _, _, cx| {
                        if let Some(terminal) = terminal.clone() {
                            desk.reveal_terminal(&terminal);
                        }
                        desk.dismiss_toast(id);
                        cx.notify();
                    })))
                })
        }))
}

fn section_label(title: &str, count: usize, colors: &Palette) -> impl IntoElement {
    div().px(px(6.)).h(px(22.)).flex().items_center().text_xs().text_color(from_hex(&colors.muted)).child(format!("{title} {count}"))
}

fn icon_button(id: &str, label: &str, text: &str, click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    Button::new(id.to_owned()).label(text.to_owned()).ghost().compact().tooltip(label.to_owned()).on_click(click)
}

fn pane_label(pane: &phux_client_runtime::control::PaneDescriptor) -> String {
    pane.title.as_deref().map(str::trim).filter(|title| !title.is_empty()).unwrap_or("terminal").to_owned()
}

fn agent_rank(agent: Option<&AgentInfo>) -> u8 {
    match agent.map(|info| info.state.as_str()).unwrap_or("unknown") {
        "blocked" => 0,
        "done" => 1,
        "working" => 2,
        "idle" => 3,
        _ => 4,
    }
}

fn strongest_agent(desk: &Desk, tab: &DeskTab) -> Option<String> {
    let order = ["blocked", "done", "working", "idle", "unknown"];
    let states: Vec<_> = layout::placements(&tab.root).into_iter().filter_map(|placement| {
        placement.terminal_id.as_ref().and_then(|id| desk.agent(id)).map(|info| info.state.clone())
    }).collect();
    order.iter().find(|state| states.iter().any(|item| item == *state)).map(|state| (*state).to_owned())
}

fn agent_swatch(state: &str) -> &'static str {
    for (name, color) in crate::settings::theme::agent_colors() {
        if name == state {
            return color;
        }
    }
    "#7f849c"
}


