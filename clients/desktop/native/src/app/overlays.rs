//! Palettes, settings, find, path picker, and confirm dialogs. Text entry uses Kit [`Input`].

use gpui::prelude::*;
use gpui::{Context, IntoElement, ParentElement, Styled, Window, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use crate::connection::path::{self as paths};

use crate::settings::theme::Palette;
use crate::settings::{self, DisplayPrefs};

use super::color::from_hex;
use super::commands::{self, CommandSpec};
use super::desk::{Desk, FindState, Modal, WindowRole};

pub fn layer(desk: &mut Desk, colors: &Palette, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    match desk.modal.clone() {
        Modal::None => div(),
        Modal::Commands => palette(desk, "Run a command…", command_items(desk), false, colors, window, cx),
        Modal::Goto => palette(desk, "Go to a terminal, agent or tab…  (type > for commands)", goto_items(desk), true, colors, window, cx),
        Modal::WindowSession => palette(desk, "Open a window on a session…", session_items(desk), false, colors, window, cx),
        Modal::Settings => settings_panel(desk, colors, window, cx),
        Modal::Path => path_panel(desk, colors, window, cx),
        Modal::Rename { tab_id } => text_dialog(desk, "Rename Tab", "Name", "", "Rename", colors, window, cx, move |desk, text, cx| {
            if desk.modal_tab() == Some(tab_id.clone()) {
                desk.apply_rename(text, cx);
            }
        }),
        Modal::Session { create, current } => session_dialog(desk, create, current, colors, window, cx),
        Modal::CloseSession { name } => confirm(
            desk,
            format!("Close {name}?"),
            "This ends the panes in that session. The window stays on this server.",
            "Close session",
            colors,
            window,
            cx,
            |desk, cx| desk.confirm_close_session(cx),
        ),
        Modal::Terminate { title, .. } => confirm(
            desk,
            format!("Terminate {title}?"),
            "This ends the process for every view and every client attached to it. Closing a pane only detaches.",
            "Terminate",
            colors,
            window,
            cx,
            |desk, cx| desk.confirm_terminate(cx),
        ),
    }
}

pub fn find_bar(desk: &mut Desk, query: String, case_sensitive: bool, colors: &Palette, window: &mut Window, cx: &mut Context<Desk>) -> gpui::AnyElement {
    let input = desk.input(window, cx, InputSlot::Find, &query);
    div()
        .absolute()
        .top(px(8.))
        .right(px(8.))
        .w(px(360.))
        .p(px(8.))
        .rounded(px(8.))
        .bg(from_hex(&colors.raised))
        .border_1()
        .border_color(from_hex(&colors.border))
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().flex_grow(1.).child(Input::new(&input).appearance(false)))
        .child(div().text_xs().text_color(from_hex(&colors.muted)).child(desk.find_status()))
        .child(Button::new("find-case").label(if case_sensitive { "Aa" } else { "aa" }).ghost().on_click(cx.listener(|desk, _, _, cx| {
            let case = !desk.find.as_ref().map(|find| find.case_sensitive).unwrap_or(false);
            let query = desk.find.as_ref().map(|find| find.query.clone()).unwrap_or_default();
            desk.run_search(query, case);
            cx.notify();
        })))
        .child(Button::new("find-prev").label("Prev").ghost().on_click(cx.listener(|desk, _, window, cx| desk.execute("find-prev", window, cx))))
        .child(Button::new("find-next").label("Next").ghost().on_click(cx.listener(|desk, _, window, cx| desk.execute("find-next", window, cx))))
        .child(Button::new("find-close").label("Close").ghost().on_click(cx.listener(|desk, _, window, cx| desk.execute("find-close", window, cx))))
        .into_any_element()
}

fn palette(
    desk: &mut Desk,
    placeholder: &str,
    items: Vec<PaletteItem>,
    goto: bool,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    let input = desk.input(window, cx, InputSlot::Palette, "");
    let query = input.read(cx).value().to_string();
    if goto && query.starts_with('>') {
        desk.modal = Modal::Commands;
    }
    let ranked = rank_palette(&items, query.trim_start_matches('>'), |item| format!("{} {}", item.title, item.detail.clone().unwrap_or_default()));
    div()
        .absolute()
        .top(px(72.))
        .left(px(0.))
        .right(px(0.))
        .flex()
        .justify_center()
        .child(
            div()
                .w(px(560.))
                .max_h(px(420.))
                .rounded(px(12.))
                .bg(from_hex(&colors.raised))
                .border_1()
                .border_color(from_hex(&colors.border))
                .id("palette-card")
                .block_mouse_except_scroll()
                .child(div().p(px(8.)).child({
                    input.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
                    Input::new(&input)
                }))
                .child(
                    div().id("palette-rows").overflow_y_scroll().p(px(6.)).children(ranked.into_iter().take(80).map(|item| {
                        let id = item.id.clone();
                        Button::new(id.clone())
                            .label(item.title.clone())
                            .ghost()
                            .on_click(cx.listener(move |desk, _, window, cx| {
                                desk.run_palette(&id, window, cx);
                            }))
                    })),
                ),
        )
}

fn settings_panel(desk: &mut Desk, colors: &Palette, window: &mut Window, cx: &mut Context<Desk>) -> gpui::Div {
    let prefs = desk.prefs.clone();
    let ghostty = settings::parse_ghostty(settings::read_ghostty_config().as_deref().unwrap_or(""));
    let themes = {
        let mut themes = crate::settings::theme::themes().to_vec();
        if let Some(extra) = settings::ghostty_theme(&ghostty) {
            themes.insert(0, extra);
        }
        themes
    };
    div().absolute().inset(px(24.)).child(
        div()
            .size_full()
            .rounded(px(12.))
            .bg(from_hex(&colors.theme.surface))
            .border_1()
            .border_color(from_hex(&colors.border))
            .id("settings-card")
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .p(px(16.))
            .gap(px(10.))
            .child(div().flex().justify_between().child("Settings").child(Button::new("settings-close").label("Close").ghost().on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx)))))
            .child(group("Theme", colors, theme_buttons(desk, &themes, &prefs, cx)))
            .child(group("Layout", colors, layout_toggles(desk, &prefs, cx)))
            .child(group("Terminal", colors, terminal_controls(desk, &prefs, cx)))
            .child(group("Keyboard", colors, shortcut_list(desk, colors)))
            .child(group("Ghostty", colors, ghostty_controls(desk, &ghostty, cx)))
            .child(group("Connection", colors, connection_facts(desk, colors, cx))),
    )
}

fn path_panel(desk: &mut Desk, colors: &Palette, window: &mut Window, cx: &mut Context<Desk>) -> gpui::Div {
    let input = desk.input(window, cx, InputSlot::Path, "");
    let state = desk.picker.clone();
    div().absolute().top(px(72.)).w_full().flex().justify_center().child(
        div()
            .w(px(640.))
            .max_h(px(480.))
            .rounded(px(12.))
            .bg(from_hex(&colors.raised))
            .border_1()
            .border_color(from_hex(&colors.border))
            .id("path-card")
            .block_mouse_except_scroll()
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child("Insert Path — host paths only, never this Mac")
            .child({
                input.update(cx, |state, cx| state.set_placeholder("Search host paths", window, cx));
                Input::new(&input)
            })
            .children(state.as_ref().map(|state| div().text_xs().text_color(from_hex(&colors.muted)).child(state.message.clone())))
            .child(div().id("path-rows").overflow_y_scroll().children(path_rows(desk, colors, cx)))
            .child(Button::new("path-close").label("Close").ghost().on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx)))),
    )
}

fn text_dialog(
    desk: &mut Desk,
    title: &str,
    label: &str,
    initial: &str,
    confirm: &str,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
    apply: impl Fn(&mut Desk, String, &mut Context<Desk>) + 'static,
) -> gpui::Div {
    let input = desk.input(window, cx, InputSlot::Rename, initial);
    div().absolute().top(px(120.)).w_full().flex().justify_center().child(
        div()
            .w(px(420.))
            .p(px(16.))
            .rounded(px(12.))
            .bg(from_hex(&colors.raised))
            .border_1()
            .border_color(from_hex(&colors.border))
            .id("rename-card")
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(title.to_owned())
            .child(label.to_owned())
            .child(Input::new(&input))
            .child(div().flex().gap(px(8.))
                .child(Button::new("rename-cancel").label("Cancel").ghost().on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))))
                .child(Button::new("rename-ok").label(confirm.to_owned()).primary().on_click(cx.listener(move |desk, _, _, cx| {
                    let text = desk.input_value(cx, InputSlot::Rename);
                    apply(desk, text, cx);
                })))),
    )
}

fn session_dialog(desk: &mut Desk, create: bool, current: String, colors: &Palette, window: &mut Window, cx: &mut Context<Desk>) -> gpui::Div {
    let name = desk.input(window, cx, InputSlot::SessionName, if create { "" } else { current.as_str() });
    let directory = desk.input(window, cx, InputSlot::SessionDir, "");
    div().absolute().top(px(120.)).w_full().flex().justify_center().child(
        div()
            .w(px(460.))
            .p(px(16.))
            .rounded(px(12.))
            .bg(from_hex(&colors.raised))
            .id("session-card")
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(if create { "New Session" } else { "Rename Session" })
            .child({
                name.update(cx, |state, cx| state.set_placeholder("Session name", window, cx));
                Input::new(&name)
            })
            .when(create, |column| column.child({
                directory.update(cx, |state, cx| state.set_placeholder("Directory", window, cx));
                Input::new(&directory)
            }))
            .child(div().flex().gap(px(8.))
                .child(Button::new("session-cancel").label("Cancel").ghost().on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))))
                .child(Button::new("session-ok").label(if create { "Create" } else { "Rename" }).primary().on_click(cx.listener(move |desk, _, _, cx| {
                    let name = desk.input_value(cx, InputSlot::SessionName);
                    let directory = desk.input_value(cx, InputSlot::SessionDir);
                    desk.apply_session(name, if create { Some(directory) } else { None }, cx);
                })))),
    )
}

fn confirm(
    desk: &Desk,
    title: String,
    body: &str,
    confirm: &str,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
    run: impl Fn(&mut Desk, &mut Context<Desk>) + 'static,
) -> gpui::Div {
    let _ = (desk, window);
    div().absolute().top(px(140.)).w_full().flex().justify_center().child(
        div()
            .w(px(440.))
            .p(px(16.))
            .rounded(px(12.))
            .bg(from_hex(&colors.raised))
            .border_1()
            .border_color(from_hex(&colors.theme.danger))
            .id("confirm-card")
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(title)
            .child(body.to_owned())
            .child(div().flex().gap(px(8.))
                .child(Button::new("confirm-cancel").label("Cancel").ghost().on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))))
                .child(Button::new("confirm-ok").label(confirm.to_owned()).danger().on_click(cx.listener(move |desk, _, _, cx| run(desk, cx))))),
    )
}

fn command_items(desk: &Desk) -> Vec<PaletteItem> {
    commands::COMMANDS.iter().filter(|command| !is_numbered_tab(command)).map(|command| PaletteItem {
        id: command.id.to_owned(),
        title: command.title.to_owned(),
        detail: desk.shown_chord(command.id),
        kind: ItemKind::Command,
    }).collect()
}

fn goto_items(desk: &Desk) -> Vec<PaletteItem> {
    let mut items = Vec::new();
    if let Some(link) = desk.link.as_ref() {
        for pane in link.connection.panes() {
            let agent = link.connection.agents.get(&pane.terminal_id);
            let title = agent.map(|info| format!("{} — {}", info.name, pane_title(pane))).unwrap_or_else(|| pane_title(pane));
            items.push(PaletteItem {
                id: format!("pane:{}", pane.terminal_id),
                title,
                detail: Some(format!("{} · {}", pane.session_name, pane.cwd.clone().unwrap_or_default())),
                kind: ItemKind::Pane(pane.terminal_id.clone()),
            });
        }
        for (index, tab) in link.workspace.tabs.iter().enumerate() {
            items.push(PaletteItem {
                id: format!("tab:{}", tab.id),
                title: super::desk::tab_title(desk, tab),
                detail: Some(format!("Tab {}", index + 1)),
                kind: ItemKind::Tab(tab.id.clone()),
            });
        }
    }
    items
}

fn session_items(desk: &Desk) -> Vec<PaletteItem> {
    desk.link.as_ref().and_then(|link| link.connection.topology.as_ref()).map(|topology| {
        topology.sessions.iter().map(|session| PaletteItem {
            id: format!("window-{}", session.id),
            title: session.name.clone(),
            detail: None,
            kind: ItemKind::WindowSession(session.name.clone()),
        }).collect()
    }).unwrap_or_default()
}

fn path_rows(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> Vec<gpui::AnyElement> {
    let Some(state) = desk.picker.clone() else { return Vec::new() };
    state.rows.iter().map(|row| {
        let path = row.path.clone();
        let directory = paths::directory_for_terminal(&row.kind, &row.path);
        div()
            .h(px(28.))
            .flex()
            .items_center()
            .justify_between()
            .child(div().text_sm().text_color(from_hex(&colors.theme.foreground)).child(paths::display_path(&row.path)))
            .child(div().flex().gap(px(4.))
                .child(Button::new(format!("insert-{}", row.path)).label("Insert").ghost().on_click(cx.listener({
                    let path = path.clone();
                    move |desk, _, _, cx| {
                        desk.insert_path(path.clone());
                        cx.notify();
                    }
                })))
                .children(directory.map(|directory| {
                    Button::new(format!("open-{}", directory)).label("New terminal").ghost().on_click(cx.listener(move |desk, _, _, cx| {
                        desk.open_path_terminal(directory.clone());
                        cx.notify();
                    }))
                })))
            .into_any_element()
    }).collect()
}

fn theme_buttons(desk: &Desk, themes: &[crate::settings::theme::Theme], prefs: &DisplayPrefs, cx: &mut Context<Desk>) -> gpui::Div {
    div().flex().flex_wrap().gap(px(6.)).children(themes.iter().map(|theme| {
        let id = theme.id.clone();
        let selected = prefs.theme_id == theme.id;
        let button = Button::new(format!("theme-{}", theme.id)).label(theme.name.clone());
        let button = if selected { button.primary() } else { button };
        button.on_click(cx.listener(move |desk, _, _, cx| {
            let id = id.clone();
            desk.update_theme(id, cx);
        }))
    }))
}

fn layout_toggles(desk: &Desk, prefs: &DisplayPrefs, cx: &mut Context<Desk>) -> gpui::Div {
    div().flex().gap(px(6.))
        .child(Button::new("pref-sidebar").label(if prefs.sidebar_visible { "Hide sidebar" } else { "Show sidebar" }).on_click(cx.listener(|desk, _, window, cx| desk.execute("sidebar", window, cx))))
        .child(Button::new("pref-notify").label(if prefs.notifications { "Notifications on" } else { "Notifications off" }).on_click(cx.listener(|desk, _, window, cx| desk.execute("notify", window, cx))))
}

fn terminal_controls(desk: &Desk, prefs: &DisplayPrefs, cx: &mut Context<Desk>) -> gpui::Div {
    div().flex().flex_col().gap(px(4.))
        .child(stepper(desk, "font", "Font size", format!("{}", prefs.font_size), cx, |desk, window, cx| desk.execute("font-up", window, cx), |desk, window, cx| desk.execute("font-down", window, cx)))
        .child(div().text_xs().child(format!("Line height {:.2}  Cell {:.0}%  Padding {}×{}  Unfocused {:.0}%  Glass {:.0}%", prefs.line_height, prefs.cell_width * 100., prefs.padding_x, prefs.padding_y, prefs.unfocused_opacity * 100., prefs.terminal_opacity * 100.)))
        .child(Button::new("line-up").label("Line height +").ghost().on_click(cx.listener(|desk, _, _, cx| desk.bump_line(0.05, cx))))
        .child(Button::new("line-down").label("Line height −").ghost().on_click(cx.listener(|desk, _, _, cx| desk.bump_line(-0.05, cx))))
        .child(Button::new("cell-up").label("Cell width +").ghost().on_click(cx.listener(|desk, _, _, cx| desk.bump_cell(0.01, cx))))
        .child(Button::new("pad-up").label("Padding +").ghost().on_click(cx.listener(|desk, _, _, cx| desk.bump_padding(1., cx))))
        .child(Button::new("glass-down").label("More glass").ghost().on_click(cx.listener(|desk, _, _, cx| desk.bump_glass(-0.05, cx))))
        .child(Button::new("dim-down").label("Dim unfocused").ghost().on_click(cx.listener(|desk, _, _, cx| desk.bump_dim(-0.05, cx))))
        .child(Button::new("option-alt").label(if prefs.option_as_alt { "Option sends Alt" } else { "Option types characters" }).on_click(cx.listener(|desk, _, window, cx| desk.execute("option-alt", window, cx))))
        .children(crate::settings::FONT_FAMILIES.iter().map(|family| {
            let family = (*family).to_owned();
            let button = Button::new(format!("font-{family}")).label(family.clone());
            let button = if prefs.font_family == family { button.primary() } else { button };
            button.on_click(cx.listener(move |desk, _, _, cx| desk.set_font(family.clone(), cx)))
        }))
}

fn shortcut_list(desk: &Desk, colors: &Palette) -> gpui::Div {
    div().flex().flex_col().gap(px(2.)).children(commands::COMMANDS.iter().filter(|command| !is_numbered_tab(command)).filter_map(|command| {
        desk.shown_chord(command.id).map(|chord| {
            div().flex().justify_between().text_sm().text_color(from_hex(&colors.theme.foreground)).child(command.title).child(chord)
        })
    }))
}

fn ghostty_controls(desk: &Desk, ghostty: &settings::GhosttyConfig, cx: &mut Context<Desk>) -> gpui::Div {
    div().flex().flex_col().gap(px(6.))
        .child(if desk.ghostty_found { "Ghostty config found" } else { "No Ghostty config found" })
        .children(ghostty.font_family.clone().map(|font| div().text_xs().child(font)))
        .child(Button::new("ghostty-apply").label("Use Ghostty look").primary().on_click(cx.listener(|desk, _, window, cx| desk.execute("import-ghostty", window, cx))))
        .child(Button::new("ghostty-reload").label("Reload config").on_click(cx.listener(|desk, _, window, cx| desk.execute("reload-config", window, cx))))
        .child(Button::new("ghostty-keys").label(if desk.prefs.ghostty_keys { "Ghostty keybinds on" } else { "Ghostty keybinds off" }).on_click(cx.listener(|desk, _, _, cx| desk.toggle_ghostty_keys(cx))))
        .children(ghostty.unmapped.iter().map(|line| div().text_xs().child(line.clone())))
}

fn connection_facts(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> gpui::Div {
    let status = desk.link.as_ref().map(|link| format!("{:?}", link.connection.status)).unwrap_or_else(|| "offline".into());
    let server = desk.link.as_ref().and_then(|link| link.connection.server_id()).unwrap_or_else(|| "—".into());
    let features = desk.link.as_ref().map(|link| link.connection.features().join(", ")).unwrap_or_default();
    div().flex().flex_col().gap(px(4.)).text_sm().text_color(from_hex(&colors.theme.foreground))
        .child(format!("Status {status}"))
        .child(format!("Socket {}", desk.socket.display()))
        .child(format!("Home session {}", desk.session))
        .child(format!("Server id {server}"))
        .child(format!("Capabilities {features}"))
        .child(Button::new("conn-reconnect").label("Reconnect").primary().on_click(cx.listener(|desk, _, window, cx| desk.execute("reconnect", window, cx))))
}

fn group(title: &str, colors: &Palette, body: impl IntoElement) -> impl IntoElement {
    div().flex().flex_col().gap(px(6.)).child(div().text_sm().font_weight(gpui::FontWeight::BOLD).text_color(from_hex(&colors.theme.foreground)).child(title.to_owned())).child(body)
}

fn stepper(
    _desk: &Desk,
    id: &str,
    label: &str,
    value: String,
    cx: &mut Context<Desk>,
    more: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
    less: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
) -> impl IntoElement {
    div().flex().items_center().gap(px(6.))
        .child(label.to_owned())
        .child(value)
        .child(Button::new(format!("{id}-less")).label("−").ghost().on_click(cx.listener(move |desk, _, window, cx| less(desk, window, cx))))
        .child(Button::new(format!("{id}-more")).label("+").ghost().on_click(cx.listener(move |desk, _, window, cx| more(desk, window, cx))))
}

fn is_numbered_tab(command: &CommandSpec) -> bool {
    command.id.starts_with("tab-") && command.id[4..].parse::<u8>().is_ok()
}

fn pane_title(pane: &phux_client_runtime::control::PaneDescriptor) -> String {
    pane.title.clone().unwrap_or_else(|| "terminal".into())
}

fn rank_palette<T: Clone>(items: &[T], query: &str, text: impl Fn(&T) -> String) -> Vec<T> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return items.to_vec();
    }
    let mut ranked = Vec::new();
    for item in items {
        let hay = text(item).to_lowercase();
        let score = if hay.starts_with(&needle) {
            0
        } else if hay.contains(&needle) {
            1
        } else {
            continue;
        };
        ranked.push((score, item.clone()));
    }
    ranked.sort_by_key(|(score, _)| *score);
    ranked.into_iter().map(|(_, item)| item).collect()
}

fn paths_message(state: &crate::connection::path::PickerState) -> String {
    state.message.clone()
}

#[derive(Clone)]
struct PaletteItem {
    id: String,
    title: String,
    detail: Option<String>,
    kind: ItemKind,
}

#[derive(Clone)]
enum ItemKind {
    Command,
    Pane(phux_protocol::ResourceId),
    Tab(String),
    WindowSession(String),
}

#[derive(Clone, Copy)]
pub enum InputSlot {
    Palette,
    Find,
    Rename,
    SessionName,
    SessionDir,
    Path,
}

pub(crate) trait OverlayHost: Sized {
    fn run_palette(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>);
    fn input(&mut self, window: &mut Window, cx: &mut Context<Self>, slot: InputSlot, initial: &str) -> gpui::Entity<InputState>;
    fn input_value(&self, cx: &gpui::App, slot: InputSlot) -> String;
    fn on_input(&mut self, slot: InputSlot, value: String, submit: bool, window: &mut Window, cx: &mut Context<Self>);
    fn update_theme(&mut self, id: String, cx: &mut Context<Self>);
    fn set_font(&mut self, family: String, cx: &mut Context<Self>);
    fn bump_line(&mut self, delta: f64, cx: &mut Context<Self>);
    fn bump_cell(&mut self, delta: f64, cx: &mut Context<Self>);
    fn bump_padding(&mut self, delta: f64, cx: &mut Context<Self>);
    fn bump_glass(&mut self, delta: f64, cx: &mut Context<Self>);
    fn bump_dim(&mut self, delta: f64, cx: &mut Context<Self>);
    fn toggle_ghostty_keys(&mut self, cx: &mut Context<Self>);
    fn modal_tab(&self) -> Option<String>;
    fn input_entity(&self, slot: InputSlot) -> Option<gpui::Entity<InputState>>;
    fn store_input(&mut self, slot: InputSlot, input: gpui::Entity<InputState>);
}

impl OverlayHost for Desk {
    fn run_palette(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = command_items(self).into_iter().chain(goto_items(self)).chain(session_items(self)).find(|item| item.id == id) {
            self.modal = Modal::None;
            match item.kind {
                ItemKind::Command => self.execute(&item.id, window, cx),
                ItemKind::Pane(terminal) => {
                    self.reveal_terminal(&terminal);
                    cx.notify();
                }
                ItemKind::Tab(tab) => {
                    self.select_tab(&tab);
                    cx.notify();
                }
                ItemKind::WindowSession(name) => self.open_session_window(name, cx),
            }
        }
    }

    fn input(&mut self, window: &mut Window, cx: &mut Context<Self>, slot: InputSlot, initial: &str) -> gpui::Entity<InputState> {
        if let Some(existing) = self.input_entity(slot) {
            return existing;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let slot_for_event = slot;
        cx.subscribe_in(&input, window, move |desk, state, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                let value = state.read(cx).value().to_string();
                desk.on_input(slot_for_event, value, matches!(event, InputEvent::PressEnter { .. }), window, cx);
            }
        }).detach();
        self.store_input(slot, input.clone());
        input
    }

    fn input_value(&self, cx: &gpui::App, slot: InputSlot) -> String {
        self.input_entity(slot).map(|input| input.read(cx).value().to_string()).unwrap_or_default()
    }

    fn on_input(&mut self, slot: InputSlot, value: String, submit: bool, window: &mut Window, cx: &mut Context<Self>) {
        match slot {
            InputSlot::Find => {
                let case = self.find.as_ref().map(|find| find.case_sensitive).unwrap_or(false);
                self.run_search(value, case);
            }
            InputSlot::Path => {
                let root = self.picker.as_ref().map(|picker| picker.root.clone()).unwrap_or_else(|| "~".into());
                self.query_paths(root, value);
            }
            InputSlot::Palette if submit => {
                let items = match self.modal {
                    Modal::Goto => goto_items(self),
                    Modal::WindowSession => session_items(self),
                    _ => command_items(self),
                };
                if let Some(item) = items.first() {
                    let id = item.id.clone();
                    self.run_palette(&id, window, cx);
                }
            }
            InputSlot::Rename | InputSlot::SessionName if submit => {}
            _ => {}
        }
        cx.notify();
    }

    fn update_theme(&mut self, id: String, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| prefs.theme_id = id, cx);
    }

    fn set_font(&mut self, family: String, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| prefs.font_family = family, cx);
    }

    fn bump_line(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| prefs.line_height += delta, cx);
    }

    fn bump_cell(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| prefs.cell_width += delta, cx);
    }

    fn bump_padding(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| {
            prefs.padding_x += delta;
            prefs.padding_y += delta;
        }, cx);
    }

    fn bump_glass(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| prefs.terminal_opacity += delta, cx);
    }

    fn bump_dim(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| prefs.unfocused_opacity += delta, cx);
    }

    fn toggle_ghostty_keys(&mut self, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| prefs.ghostty_keys = !prefs.ghostty_keys, cx);
    }

    fn modal_tab(&self) -> Option<String> {
        match &self.modal {
            Modal::Rename { tab_id } => Some(tab_id.clone()),
            _ => None,
        }
    }


    fn input_entity(&self, slot: InputSlot) -> Option<gpui::Entity<InputState>> {
        self.inputs.get(slot as usize).cloned().flatten()
    }

    fn store_input(&mut self, slot: InputSlot, input: gpui::Entity<InputState>) {
        let index = slot as usize;
        if self.inputs.len() <= index {
            self.inputs.resize_with(index + 1, || None);
        }
        self.inputs[index] = Some(input);
    }
}
