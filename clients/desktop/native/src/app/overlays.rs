//! Palettes, the path picker, dialogs, and the find bar. Lists are the Kit
//! [`Command`] palette; text entry is Kit [`Input`]. Every modal sits on a
//! backdrop that closes it on click, and its control takes the keyboard on open.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, MouseButton, ParentElement, Styled,
    Subscription, Window, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, IndexPath, Sizable};
use phux_protocol::ResourceId;
use phux_protocol::ids::SessionKey;

use crate::connection::path::{self as paths};
use crate::settings::theme::Palette;

use super::actions::Run;
use super::color::from_hex;
use super::desk::{self, Desk, Modal};

/// Text fields a modal or the find bar owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSlot {
    Find,
    Rename,
    SessionName,
    SessionDir,
    HostTarget,
    HostName,
}

/// Build the open modal's controls with fresh or prefilled values and give it the keyboard.
pub(crate) fn prepare(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    match desk.modal.clone() {
        Modal::None => {}
        Modal::Commands
        | Modal::Goto
        | Modal::WindowSession
        | Modal::MoveResources
        | Modal::Path => {
            let state = palette_state(desk, window, cx);
            state.update(cx, |state, cx| {
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
        }
        Modal::Settings => {
            super::settings_panel::prepare(desk, window, cx);
            window.focus(&desk.focus, cx);
        }
        Modal::Rename { tab_id } => {
            let current = desk
                .link
                .as_ref()
                .and_then(|link| link.workspace.tabs.iter().find(|tab| tab.id == tab_id))
                .map(|tab| desk::tab_title(desk, tab))
                .unwrap_or_default();
            let input = modal_input(
                desk,
                InputSlot::Rename,
                &current,
                "Follow the terminal title",
                window,
                cx,
            );
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::Session {
            create, current, ..
        } => {
            let name = modal_input(
                desk,
                InputSlot::SessionName,
                if create { "" } else { &current },
                "Session name",
                window,
                cx,
            );
            if create {
                let directory = desk.focused_cwd().unwrap_or_default();
                modal_input(
                    desk,
                    InputSlot::SessionDir,
                    &directory,
                    "Directory",
                    window,
                    cx,
                );
            }
            name.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::AddHost => {
            let target = modal_input(
                desk,
                InputSlot::HostTarget,
                "",
                "ssh destination, e.g. me@mini",
                window,
                cx,
            );
            modal_input(
                desk,
                InputSlot::HostName,
                "",
                "Name (optional, defaults to the host)",
                window,
                cx,
            );
            target.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::KillSession { .. }
        | Modal::NativeLaunch
        | Modal::Terminate { .. }
        | Modal::RemoveHost { .. }
        | Modal::Notifications => window.focus(&desk.focus, cx),
    }
}

fn palette_state(
    desk: &mut Desk,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> Entity<CommandState> {
    if let Some(state) = desk.palette_state.clone() {
        return state;
    }
    let state = cx.new(|cx| CommandState::new(window, cx));
    desk.palette_state = Some(state.clone());
    state
}

fn modal_input(
    desk: &mut Desk,
    slot: InputSlot,
    initial: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> Entity<InputState> {
    let (input, subscription) = input_with(slot, initial, placeholder, window, cx);
    desk.modal_inputs.push((slot, input.clone(), subscription));
    input
}

/// The find field, focused, reporting each edit and Enter / Shift-Enter.
pub(crate) fn find_input(
    _desk: &mut Desk,
    initial: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> (Entity<InputState>, Subscription) {
    let (input, subscription) =
        input_with(InputSlot::Find, initial, "Find in terminal", window, cx);
    input.update(cx, |input, cx| input.focus(window, cx));
    (input, subscription)
}

fn input_with(
    slot: InputSlot,
    initial: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> (Entity<InputState>, Subscription) {
    let initial = initial.to_owned();
    let placeholder = placeholder.to_owned();
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(initial)
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |desk, state, event: &InputEvent, window, cx| {
            let value = state.read(cx).value().to_string();
            match event {
                InputEvent::Change => desk.on_input(slot, value, None, window, cx),
                InputEvent::PressEnter { shift, .. } => {
                    desk.on_input(slot, value, Some(*shift), window, cx)
                }
                _ => {}
            }
        },
    );
    (input, subscription)
}

impl Desk {
    fn input_value(&self, slot: InputSlot, cx: &App) -> String {
        self.modal_inputs
            .iter()
            .find(|(owned, _, _)| *owned == slot)
            .map(|(_, input, _)| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn input_entity(&self, slot: InputSlot) -> Option<Entity<InputState>> {
        self.modal_inputs
            .iter()
            .find(|(owned, _, _)| *owned == slot)
            .map(|(_, input, _)| input.clone())
    }

    /// A field edit (`submit` is `None`) or Enter (`Some(shift)`).
    fn on_input(
        &mut self,
        slot: InputSlot,
        value: String,
        submit: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (slot, submit) {
            (InputSlot::Find, None) => {
                let case = self
                    .find
                    .as_ref()
                    .map(|find| find.case_sensitive)
                    .unwrap_or(false);
                self.run_search(value, case);
            }
            (InputSlot::Find, Some(shift)) => {
                self.step_find(if shift { -1 } else { 1 }, window, cx)
            }
            (InputSlot::Rename, Some(_)) => self.apply_rename(value, window, cx),
            (InputSlot::SessionName | InputSlot::SessionDir, Some(_)) => {
                let name = self.input_value(InputSlot::SessionName, cx);
                let directory = self
                    .input_entity(InputSlot::SessionDir)
                    .map(|input| input.read(cx).value().to_string());
                self.apply_session(name, directory, window, cx);
            }
            (InputSlot::HostTarget | InputSlot::HostName, Some(_)) => {
                let target = self.input_value(InputSlot::HostTarget, cx);
                let name = self.input_value(InputSlot::HostName, cx);
                self.apply_add_host(target, name, window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// The palette query changed. In Go To, a leading `>` switches to commands
    /// and keeps the rest of the query; the path picker searches the host.
    fn palette_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.modal {
            Modal::Goto if query.starts_with('>') => {
                self.modal = Modal::Commands;
                self.mark_prepared();
                let rest = query.trim_start_matches('>').trim_start().to_owned();
                if let Some(state) = self.palette_state.clone() {
                    state.update(cx, |state, cx| state.set_query(rest, window, cx));
                }
            }
            Modal::Path => {
                let Some(picker) = self.picker.as_ref() else {
                    return;
                };
                if picker.query != query {
                    let root = picker.root.clone();
                    self.query_paths(root, query.to_owned());
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Run a confirmed palette row. Commands already ran through their [`Run`] action.
    fn pick(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            Target::Command => {}
            Target::Pane(terminal) => {
                self.close_modal(window, cx);
                self.reveal_terminal(&terminal);
                self.after_command(window, cx);
            }
            Target::Tab(tab) => {
                self.close_modal(window, cx);
                self.select_tab(&tab);
                self.after_command(window, cx);
            }
            Target::Session(name) => self.open_session_window(name, window, cx),
            Target::Move(key, name) => {
                let selected = self.organization.selected.clone();
                self.move_resources_to(selected, key, name, window, cx);
            }
            Target::Path(item) => match item {
                PathItem::Parent(directory) => self.browse_path(directory, window, cx),
                PathItem::Row { path, .. } => self.insert_path(path, window, cx),
            },
        }
    }
}

/// What a palette row does when confirmed.
#[derive(Clone)]
enum Target {
    Command,
    Pane(ResourceId),
    Tab(String),
    Session(String),
    Move(SessionKey, String),
    Path(PathItem),
}

/// A row of the path picker: the parent directory, or a host path.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PathItem {
    Parent(String),
    Row { path: String, kind: String },
}

impl PathItem {
    /// The directory Tab / Right browses into, if this row is one.
    pub(crate) fn directory(&self) -> Option<String> {
        match self {
            Self::Parent(directory) => Some(directory.clone()),
            Self::Row { path, kind } => paths::directory_for_terminal(kind, path),
        }
    }
}

/// The picker's rows in display order, which is also the palette's row index.
pub(crate) fn path_items(desk: &Desk) -> Vec<PathItem> {
    let Some(picker) = desk.picker.as_ref() else {
        return Vec::new();
    };
    picker
        .parent
        .clone()
        .map(PathItem::Parent)
        .into_iter()
        .chain(picker.rows.iter().map(|row| PathItem::Row {
            path: row.path.clone(),
            kind: row.kind.clone(),
        }))
        .collect()
}

pub fn layer(
    desk: &mut Desk,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let card = match desk.modal.clone() {
        Modal::None => return div().into_any_element(),
        Modal::Commands | Modal::Goto | Modal::WindowSession | Modal::MoveResources => {
            palette(desk, colors, cx)
        }
        Modal::Path => path_panel(desk, colors, window, cx),
        Modal::Settings => super::settings_panel::panel(desk, colors, window, cx),
        Modal::Notifications => notifications_panel(desk, colors, cx),
        Modal::NativeLaunch => super::native_launch::panel(desk, colors, cx),
        Modal::Rename { .. } => rename_dialog(desk, colors, cx),
        Modal::Session { create, .. } => session_dialog(desk, create, colors, cx),
        Modal::KillSession { name, .. } => confirm(
            format!("Kill {name}?"),
            "This ends every process in the session, for every client, and deletes it. Closing a pane, tab, or window never does this.",
            "Kill Session",
            colors,
            cx,
            |desk, window, cx| desk.confirm_kill_session(window, cx),
        ),
        Modal::AddHost => add_host_dialog(desk, colors, cx),
        Modal::RemoveHost { name } => confirm(
            format!("Remove {name}?"),
            "This forgets the machine here. Its server and terminals keep running; `phux host add` registers it again.",
            "Remove Host",
            colors,
            cx,
            |desk, window, cx| desk.confirm_remove_host(window, cx),
        ),
        Modal::Terminate { title, .. } => confirm(
            format!("Terminate {title}?"),
            "This ends the process for every view and every client attached to it. Closing a pane only detaches.",
            "Terminate",
            colors,
            cx,
            |desk, window, cx| desk.confirm_terminate(window, cx),
        ),
    };
    backdrop(colors, cx, card)
}

/// A dimmed layer over the window that blocks the terminal and closes the modal on click.
fn backdrop(colors: &Palette, cx: &mut Context<Desk>, card: impl IntoElement) -> AnyElement {
    div()
        .id("modal-backdrop")
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .bg(from_hex(&colors.backdrop))
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|desk, _, window, cx| desk.close_modal(window, cx)),
        )
        .child(card)
        .into_any_element()
}

fn card(colors: &Palette, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .rounded(px(12.))
        .bg(from_hex(&colors.raised))
        .border_1()
        .border_color(from_hex(&colors.border))
        .shadow_lg()
        .occlude()
}

pub fn find_bar(
    desk: &Desk,
    case_sensitive: bool,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some((input, _)) = desk.find_input.as_ref() else {
        return div().into_any_element();
    };
    div()
        .absolute()
        .top(px(8.))
        .right(px(8.))
        .w(px(380.))
        .p(px(6.))
        .rounded(px(8.))
        .bg(from_hex(&colors.raised))
        .border_1()
        .border_color(from_hex(&colors.border))
        .shadow_md()
        .occlude()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(
            div().flex_grow(1.).child(
                Input::new(input)
                    .appearance(false)
                    .prefix(Icon::new(IconName::Search).small()),
            ),
        )
        .child(
            div()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child(desk.find_status()),
        )
        .child(
            Button::new("find-case")
                .icon(IconName::CaseSensitive)
                .ghost()
                .small()
                .toggled(case_sensitive)
                .tooltip("Match case")
                .on_click(cx.listener(|desk, _, window, cx| {
                    let case = !desk
                        .find
                        .as_ref()
                        .map(|find| find.case_sensitive)
                        .unwrap_or(false);
                    let query = desk
                        .find
                        .as_ref()
                        .map(|find| find.query.clone())
                        .unwrap_or_default();
                    desk.run_search(query, case);
                    desk.after_command(window, cx);
                })),
        )
        .child(
            Button::new("find-prev")
                .icon(IconName::ChevronUp)
                .ghost()
                .small()
                .tooltip("Previous match")
                .on_click(cx.listener(|desk, _, window, cx| desk.execute("find-prev", window, cx))),
        )
        .child(
            Button::new("find-next")
                .icon(IconName::ChevronDown)
                .ghost()
                .small()
                .tooltip("Next match")
                .on_click(cx.listener(|desk, _, window, cx| desk.execute("find-next", window, cx))),
        )
        .child(
            Button::new("find-close")
                .icon(IconName::Close)
                .ghost()
                .small()
                .tooltip("Close")
                .on_click(
                    cx.listener(|desk, _, window, cx| desk.execute("find-close", window, cx)),
                ),
        )
        .into_any_element()
}

/// One palette section: an optional heading and its rows.
struct Section {
    label: Option<&'static str>,
    rows: Vec<Row>,
}

struct Row {
    title: String,
    detail: Option<String>,
    /// Extra searchable text, not shown.
    keywords: String,
    target: Target,
    command: Option<&'static str>,
}

fn palette(desk: &mut Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    let Some(state) = desk.palette_state.clone() else {
        return div().into_any_element();
    };
    let query = state.read(cx).query(cx).to_string();
    let (placeholder, sections) = match desk.modal {
        Modal::Goto => (
            "Go to a terminal, agent or tab…  (type > for commands)",
            goto_sections(desk),
        ),
        Modal::WindowSession => ("Open a window on a session…", session_sections(desk)),
        Modal::MoveResources => ("Move to…", move_sections(desk)),
        _ => ("Run a command…", command_sections(desk)),
    };
    let sections = rank_sections(sections, &query);
    let count: usize = sections.iter().map(|section| section.rows.len()).sum();
    let targets: Rc<Vec<Vec<Target>>> = Rc::new(
        sections
            .iter()
            .map(|section| section.rows.iter().map(|row| row.target.clone()).collect())
            .collect(),
    );
    let desk_handle = cx.weak_entity();
    let confirm_handle = desk_handle.clone();
    let muted = from_hex(&colors.muted);
    let confirmation_hint = if desk.modal == Modal::MoveResources {
        "↑↓ select · ↩ move · esc close"
    } else {
        "↑↓ select · ↩ open · esc close"
    };
    let mut command = Command::new(&state)
        .filterable(false)
        .bordered(false)
        .placeholder(placeholder)
        .max_h(px(420.))
        .empty(|_, _, cx| {
            div()
                .py(px(24.))
                .w_full()
                .text_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No matches. Try a shorter search.")
        })
        .footer(move |_, _, _| {
            div()
                .px(px(12.))
                .py(px(6.))
                .flex()
                .justify_between()
                .text_xs()
                .text_color(muted)
                .child(format!(
                    "{count} {}",
                    if count == 1 { "result" } else { "results" }
                ))
                .child(confirmation_hint)
        })
        .on_query(move |query, window, cx| {
            let _ = desk_handle.update(cx, |desk, cx| desk.palette_query(query, window, cx));
        })
        .on_confirm(move |index: IndexPath, window, cx| {
            let target = targets
                .get(index.section)
                .and_then(|rows| rows.get(index.row))
                .cloned();
            if let Some(target) = target {
                let _ = confirm_handle.update(cx, |desk, cx| desk.pick(target, window, cx));
            }
        });
    for section in sections {
        let mut group = CommandGroup::new();
        if let Some(label) = section.label {
            group = group.label(label);
        }
        command = command.group(group.items(section.rows.into_iter().map(row_item)));
    }
    card(colors, 600.)
        .mt(px(72.))
        .overflow_hidden()
        .child(command)
        .into_any_element()
}

fn row_item(row: Row) -> CommandItem {
    let mut item = CommandItem::new()
        .label(row.title.clone())
        .keywords([row.keywords]);
    if let Some(id) = row.command {
        return item.action(Box::new(Run::new(id)));
    }
    if let Some(detail) = row.detail {
        let title = row.title;
        item = item.child(move |_, cx| {
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_baseline()
                .gap(px(8.))
                .child(div().flex_shrink_0().child(title.clone()))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail.clone()),
                )
        });
    }
    item
}

fn command_sections(desk: &Desk) -> Vec<Section> {
    let organizing = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.organization());
    let mut sections: Vec<Section> = Vec::new();
    for command in crate::settings::builtin_commands()
        .iter()
        .filter(|command| command.is_listed() && (organizing || !command.needs_organization()))
    {
        let row = Row {
            title: command.title.to_owned(),
            detail: None,
            keywords: format!("{} {}", command.group, command.id),
            target: Target::Command,
            command: Some(command.id),
        };
        match sections.last_mut() {
            Some(section) if section.label == Some(command.group) => section.rows.push(row),
            _ => sections.push(Section {
                label: Some(command.group),
                rows: vec![row],
            }),
        }
    }
    sections
}

fn goto_sections(desk: &Desk) -> Vec<Section> {
    let Some(link) = desk.link.as_ref() else {
        return Vec::new();
    };
    let mut agents = Vec::new();
    let mut terminals = Vec::new();
    for pane in link.connection.panes() {
        let agent = link.connection.agents.get(&pane.terminal_id);
        let title = desk.pane_title(Some(&pane.terminal_id));
        let detail = format!(
            "{} · {}",
            pane.session_name,
            desk::short_path(pane.cwd.clone())
        );
        let row = Row {
            title: agent
                .map(|info| format!("{} — {title}", info.name))
                .unwrap_or(title),
            keywords: agent
                .map(|info| format!("{detail} {}", info.word()))
                .unwrap_or_else(|| detail.clone()),
            detail: Some(match agent.map(|info| info.word()) {
                Some(word) if !word.is_empty() => format!("{word} · {detail}"),
                _ => detail,
            }),
            target: Target::Pane(pane.terminal_id.clone()),
            command: None,
        };
        if agent.is_some() {
            agents.push(row)
        } else {
            terminals.push(row)
        }
    }
    // A one-pane tab is already listed as its terminal.
    let tabs = link
        .workspace
        .tabs
        .iter()
        .enumerate()
        .filter_map(|(index, tab)| {
            let panes = crate::workspace::layout::placements(&tab.root).len();
            if panes < 2 {
                return None;
            }
            let detail = format!("Tab {} · {panes} panes", index + 1);
            Some(Row {
                title: desk::tab_title(desk, tab),
                keywords: detail.clone(),
                detail: Some(detail),
                target: Target::Tab(tab.id.clone()),
                command: None,
            })
        })
        .collect::<Vec<_>>();
    [("Agents", agents), ("Terminals", terminals), ("Tabs", tabs)]
        .into_iter()
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(label, rows)| Section {
            label: Some(label),
            rows,
        })
        .collect()
}

fn session_sections(desk: &Desk) -> Vec<Section> {
    let Some(link) = desk.link.as_ref() else {
        return Vec::new();
    };
    let Some(topology) = link.connection.topology.as_ref() else {
        return Vec::new();
    };
    let rows = topology
        .sessions
        .iter()
        .map(|session| {
            let panes = link
                .connection
                .panes()
                .iter()
                .filter(|pane| pane.session_id == session.id)
                .count();
            let detail = format!(
                "{panes} {}",
                if panes == 1 { "terminal" } else { "terminals" }
            );
            let detail = if session.key.is_some() && session.key == topology.inbox {
                format!("Intake · unorganized work · {detail}")
            } else if session.awaiting_organization {
                format!("Imported · awaiting organization · {detail}")
            } else {
                detail
            };
            Row {
                title: session.name.clone(),
                keywords: detail.clone(),
                detail: Some(detail),
                target: Target::Session(session.name.clone()),
                command: None,
            }
        })
        .collect();
    vec![Section {
        label: Some("Sessions"),
        rows,
    }]
}

fn move_sections(desk: &Desk) -> Vec<Section> {
    let Some(client) = desk.client() else {
        return Vec::new();
    };
    let replica = client.organization();
    let selected = &desk.organization.selected;
    let destinations: Vec<_> = replica
        .sessions()
        .iter()
        .filter(|session| {
            selected.iter().any(|key| {
                replica
                    .resource(key)
                    .is_some_and(|resource| resource.session != session.key)
            })
        })
        .collect();
    move_destination_sections(&destinations, replica.intake(), selected.len())
}

fn move_destination_sections(
    sessions: &[&phux_protocol::wire::frame::SessionRecord],
    intake: Option<SessionKey>,
    count: usize,
) -> Vec<Section> {
    let mut unorganized = Vec::new();
    let mut named = Vec::new();
    for session in sessions {
        let designated = Some(session.key) == intake;
        let row = move_destination_row(session, designated, count);
        if designated {
            unorganized.push(row);
        } else {
            named.push(row);
        }
    }
    [("Unorganized work", unorganized), ("Named sessions", named)]
        .into_iter()
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(label, rows)| Section {
            label: Some(label),
            rows,
        })
        .collect()
}

fn move_destination_row(
    session: &phux_protocol::wire::frame::SessionRecord,
    designated: bool,
    count: usize,
) -> Row {
    let (title, name) = if designated {
        ("Leave unorganized (Intake)".to_owned(), "Intake".to_owned())
    } else {
        (session.name.clone(), session.name.clone())
    };
    let imported = if !designated && session.awaiting_organization {
        " · imported session still awaiting organization"
    } else {
        ""
    };
    Row {
        title,
        detail: Some(format!(
            "Move {count} selected · execution unchanged{imported}"
        )),
        keywords: session.name.clone(),
        target: Target::Move(session.key, name),
        command: None,
    }
}

/// Rank every row against `query` as one flat list; an empty query keeps the sections.
fn rank_sections(sections: Vec<Section>, query: &str) -> Vec<Section> {
    let query = query.trim();
    if query.is_empty() {
        return sections;
    }
    let rows: Vec<Row> = sections
        .into_iter()
        .flat_map(|section| {
            let group = section.label.unwrap_or("");
            section.rows.into_iter().map(move |mut row| {
                row.keywords = format!("{} {group}", row.keywords);
                row
            })
        })
        .collect();
    let ranked = rank(rows, query, |row| {
        format!(
            "{} {} {}",
            row.title,
            row.detail.clone().unwrap_or_default(),
            row.keywords
        )
    });
    vec![Section {
        label: None,
        rows: ranked,
    }]
}

/// Palette ranking. Every whitespace-separated token must match the item's
/// text, as a substring or failing that as an in-order subsequence. Substring
/// hits at a word start rank highest; scattered subsequences rank lowest.
/// Equal scores keep their original order.
pub(crate) fn rank<T>(items: Vec<T>, query: &str, text: impl Fn(&T) -> String) -> Vec<T> {
    let lowered = query.to_lowercase();
    let tokens: Vec<&str> = lowered.split_whitespace().collect();
    if tokens.is_empty() {
        return items;
    }
    let mut ranked: Vec<(i64, T)> = items
        .into_iter()
        .filter_map(|item| {
            match_score(&text(&item).to_lowercase(), &tokens).map(|score| (score, item))
        })
        .collect();
    ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    ranked.into_iter().map(|(_, item)| item).collect()
}

fn match_score(haystack: &str, tokens: &[&str]) -> Option<i64> {
    tokens.iter().try_fold(0, |total, token| {
        token_score(haystack, token).map(|score| total + score)
    })
}

fn token_score(haystack: &str, token: &str) -> Option<i64> {
    if let Some(index) = haystack.find(token) {
        let before = haystack[..index].chars().next_back();
        let word_start = before.is_none_or(|ch| ch.is_whitespace() || "/:._-".contains(ch));
        let offset = haystack[..index].chars().count().min(60) as i64;
        return Some(if word_start { 200 } else { 120 } - offset);
    }
    let chars: Vec<char> = haystack.chars().collect();
    let mut cursor = 0;
    let mut gaps = 0;
    for wanted in token.chars() {
        let found = chars[cursor..].iter().position(|ch| *ch == wanted)?;
        gaps += found as i64;
        cursor += found + 1;
    }
    Some((60 - gaps).max(1))
}

fn path_panel(
    desk: &mut Desk,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(state) = desk.palette_state.clone() else {
        return div().into_any_element();
    };
    let Some(picker) = desk.picker.clone() else {
        return div().into_any_element();
    };
    // The search field's spinner follows the outstanding host query.
    let pending = picker.pending.is_some();
    if state.read(cx).is_loading() != pending {
        state.update(cx, |state, cx| state.set_loading(pending, window, cx));
    }
    let items = path_items(desk);
    let targets: Rc<Vec<Target>> = Rc::new(items.iter().cloned().map(Target::Path).collect());
    let desk_handle = cx.weak_entity();
    let confirm_handle = desk_handle.clone();
    let terminal_handle = desk_handle.clone();
    let muted = from_hex(&colors.muted);
    let message = paths::display_path(&picker.message);
    let rows = items.into_iter().map(move |item| {
        let terminal_handle = terminal_handle.clone();
        let (icon, label, directory) = match &item {
            PathItem::Parent(_) => (IconName::ArrowUp, ".. (parent)".to_owned(), None),
            PathItem::Row { path, kind } => (
                if kind == "directory" {
                    IconName::Folder
                } else {
                    IconName::File
                },
                paths::display_path(path),
                item.directory(),
            ),
        };
        let search = label.clone();
        CommandItem::new().label(search).child(move |_, cx| {
            let terminal_handle = terminal_handle.clone();
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(8.))
                .child(
                    Icon::new(icon.clone())
                        .small()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                .children(
                    directory
                        .clone()
                        .filter(|_| !matches!(icon, IconName::ArrowUp))
                        .map(|directory| {
                            Button::new(format!("open-{directory}"))
                                .label("New terminal here")
                                .ghost()
                                .xsmall()
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    let directory = directory.clone();
                                    let _ = terminal_handle.update(cx, |desk, cx| {
                                        desk.open_path_terminal(directory, window, cx)
                                    });
                                })
                        }),
                )
        })
    });
    let root = picker.root.clone();
    let command = Command::new(&state)
        .filterable(false)
        .bordered(false)
        .placeholder(format!("Search in {root}…"))
        .max_h(px(420.))
        .items(rows)
        .empty(move |_, _, cx| {
            div()
                .py(px(24.))
                .w_full()
                .text_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if pending { "Searching…" } else { "No paths." })
        })
        .footer(move |_, _, _| {
            div()
                .px(px(12.))
                .py(px(6.))
                .flex()
                .justify_between()
                .gap(px(12.))
                .text_xs()
                .text_color(muted)
                .child(div().min_w_0().truncate().child(message.clone()))
                .child(
                    div()
                        .flex_shrink_0()
                        .child("↩ insert · ⇥ open folder · esc close"),
                )
        })
        .on_query(move |query, window, cx| {
            let _ = desk_handle.update(cx, |desk, cx| desk.palette_query(query, window, cx));
        })
        .on_confirm(move |index: IndexPath, window, cx| {
            if let Some(target) = targets.get(index.row).cloned() {
                let _ = confirm_handle.update(cx, |desk, cx| desk.pick(target, window, cx));
            }
        });
    card(colors, 640.)
        .mt(px(72.))
        .overflow_hidden()
        .key_context("PathPicker")
        .child(command)
        .into_any_element()
}

/// Every notification this window has posted, newest first. Unread entries
/// carry a dot until the list closes; a click reveals the entry's terminal.
fn notification_rows(
    desk: &Desk,
    colors: &Palette,
    cx: &mut Context<Desk>,
    agents: bool,
) -> Vec<AnyElement> {
    desk.feed
        .items()
        .filter(|entry| (entry.kind == super::notifications::Kind::Agent) == agents)
        .map(|entry| {
            let id = entry.id;
            let edge = from_hex(&super::view::notification_edge(entry, colors));
            let age = super::view::ago(0, entry.at.elapsed().as_millis() as u64);
            div()
                .id(("notification", id as usize))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .flex()
                .items_start()
                .gap(px(10.))
                .cursor_pointer()
                .hover(|style| style.bg(from_hex(&colors.hover)))
                .child(
                    div()
                        .mt(px(5.))
                        .size(px(7.))
                        .flex_shrink_0()
                        .rounded_full()
                        .when(entry.read, |dot| dot.border_1().border_color(edge))
                        .when(!entry.read, |dot| dot.bg(edge)),
                )
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
                                .when(!entry.read, |title| {
                                    title.font_weight(gpui::FontWeight::MEDIUM)
                                })
                                .text_color(from_hex(&colors.theme.foreground))
                                .child(entry.title.clone()),
                        )
                        .children(entry.body.clone().map(|body| {
                            div()
                                .text_xs()
                                .line_clamp(2)
                                .text_color(from_hex(&colors.muted))
                                .child(body)
                        })),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(from_hex(&colors.subtext))
                        .child(age),
                )
                .on_click(cx.listener(move |desk, _, window, cx| {
                    desk.open_notification(id, window, cx);
                }))
                .into_any_element()
        })
        .collect()
}

fn notification_heading(title: &str, unread: usize, colors: &Palette) -> AnyElement {
    div()
        .px(px(10.))
        .py(px(6.))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(from_hex(&colors.muted))
        .child(format!("{title} · {unread} unread alerts"))
        .into_any_element()
}

fn notifications_panel(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    let agent_rows = notification_rows(desk, colors, cx, true);
    let general_rows = notification_rows(desk, colors, cx, false);
    let unread_agents = desk
        .feed
        .unread()
        .filter(|entry| entry.kind == super::notifications::Kind::Agent)
        .count();
    let unread_general = desk.feed.unread().count() - unread_agents;
    let empty = desk.feed.is_empty();
    let mut rows = vec![notification_heading(
        "Inbox — agent attention",
        unread_agents,
        colors,
    )];
    if agent_rows.is_empty() {
        rows.push(
            div()
                .px(px(10.))
                .py(px(8.))
                .text_sm()
                .text_color(from_hex(&colors.muted))
                .child("No agent attention alerts.")
                .into_any_element(),
        );
    }
    rows.extend(agent_rows);
    if !general_rows.is_empty() {
        rows.push(notification_heading(
            "Notifications",
            unread_general,
            colors,
        ));
        rows.extend(general_rows);
    }
    card(colors, 460.)
        .mt(px(60.))
        .p(px(8.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .px(px(8.))
                .py(px(4.))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(from_hex(&colors.theme.foreground))
                        .child("Inbox & notifications"),
                )
                .when(!empty, |header| {
                    header.child(
                        Button::new("notifications-clear")
                            .label("Clear all")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|desk, _, window, cx| {
                                desk.execute("clear-notifications", window, cx)
                            })),
                    )
                }),
        )
        .child(
            div()
                .px(px(10.))
                .py(px(4.))
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child("Inbox holds agent attention alerts; notifications hold errors and command results. Click an agent alert to open its terminal."),
        )
        .child(if empty {
            div()
                .px(px(10.))
                .py(px(16.))
                .text_sm()
                .text_color(from_hex(&colors.muted))
                .child("No alerts or notifications yet. Unorganized work lives in Intake.")
                .into_any_element()
        } else {
            div()
                .id("notification-list")
                .max_h(px(480.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows)
                .into_any_element()
        })
        .into_any_element()
}

fn dialog_card(title: &str, colors: &Palette) -> gpui::Div {
    card(colors, 440.)
        .mt(px(120.))
        .p(px(16.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(from_hex(&colors.theme.foreground))
                .child(title.to_owned()),
        )
}

fn dialog_buttons(confirm: Button, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .flex()
        .justify_end()
        .gap(px(8.))
        .child(
            Button::new("dialog-cancel")
                .label("Cancel")
                .ghost()
                .on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))),
        )
        .child(confirm)
}

fn rename_dialog(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    let Some(input) = desk.input_entity(InputSlot::Rename) else {
        return div().into_any_element();
    };
    dialog_card("Rename Tab", colors)
        .child(
            div()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child("Leave it blank to follow the terminal title."),
        )
        .child(Input::new(&input))
        .child(dialog_buttons(
            Button::new("rename-ok")
                .label("Rename")
                .primary()
                .on_click(cx.listener(|desk, _, window, cx| {
                    let title = desk.input_value(InputSlot::Rename, cx);
                    desk.apply_rename(title, window, cx);
                })),
            cx,
        ))
        .into_any_element()
}

fn session_dialog(
    desk: &Desk,
    create: bool,
    colors: &Palette,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(name) = desk.input_entity(InputSlot::SessionName) else {
        return div().into_any_element();
    };
    let directory = desk.input_entity(InputSlot::SessionDir);
    dialog_card(
        if create {
            "New Session"
        } else {
            "Rename Session"
        },
        colors,
    )
    .child(Input::new(&name))
    .children(directory.map(|directory| Input::new(&directory)))
    .child(dialog_buttons(
        Button::new("session-ok")
            .label(if create { "Create" } else { "Rename" })
            .primary()
            .on_click(cx.listener(|desk, _, window, cx| {
                let name = desk.input_value(InputSlot::SessionName, cx);
                let directory = desk
                    .input_entity(InputSlot::SessionDir)
                    .map(|input| input.read(cx).value().to_string());
                desk.apply_session(name, directory, window, cx);
            })),
        cx,
    ))
    .into_any_element()
}

fn add_host_dialog(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    let (Some(target), Some(name)) = (
        desk.input_entity(InputSlot::HostTarget),
        desk.input_entity(InputSlot::HostName),
    ) else {
        return div().into_any_element();
    };
    dialog_card("Add Host", colors)
        .child(
            div()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child("Uses the ssh access you already have. phux must be installed on the host; enrollment starts its server, pairs, and finds a direct route."),
        )
        .child(Input::new(&target))
        .child(Input::new(&name))
        .child(dialog_buttons(
            Button::new("host-add-ok")
                .label("Add Host")
                .primary()
                .on_click(cx.listener(|desk, _, window, cx| {
                    let target = desk.input_value(InputSlot::HostTarget, cx);
                    let name = desk.input_value(InputSlot::HostName, cx);
                    desk.apply_add_host(target, name, window, cx);
                })),
            cx,
        ))
        .into_any_element()
}

fn confirm(
    title: String,
    body: &str,
    label: &str,
    colors: &Palette,
    cx: &mut Context<Desk>,
    run: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
) -> AnyElement {
    dialog_card(&title, colors)
        .border_color(from_hex(&colors.theme.danger))
        .child(
            div()
                .text_sm()
                .text_color(from_hex(&colors.subtext))
                .child(body.to_owned()),
        )
        .child(dialog_buttons(
            Button::new("confirm-ok")
                .label(label.to_owned())
                .danger()
                .on_click(cx.listener(move |desk, _, window, cx| run(desk, window, cx))),
            cx,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Target, move_destination_sections, rank, rank_sections};
    use phux_protocol::ids::{Revision, SessionKey};
    use phux_protocol::wire::frame::SessionRecord;

    fn session(byte: u8, name: &str) -> SessionRecord {
        SessionRecord::new(SessionKey::new([byte; 16]).unwrap(), name, Revision::new(1))
    }

    #[test]
    fn move_intake_is_an_explicit_destination_not_a_named_session() {
        let intake = session(1, "legacy-home");
        let work = session(2, "Intake");
        let sections = move_destination_sections(&[&work, &intake], Some(intake.key), 2);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].label, Some("Unorganized work"));
        assert_eq!(sections[0].rows[0].title, "Leave unorganized (Intake)");
        assert!(
            sections[0].rows[0]
                .detail
                .as_ref()
                .unwrap()
                .contains("execution unchanged")
        );
        assert!(
            matches!(&sections[0].rows[0].target, Target::Move(key, name) if *key == intake.key && name == "Intake")
        );
        assert_eq!(sections[1].label, Some("Named sessions"));
        assert_eq!(sections[1].rows[0].title, "Intake");
        assert!(matches!(&sections[1].rows[0].target, Target::Move(key, _) if *key == work.key));
    }

    #[test]
    fn missing_or_ineligible_intake_never_fabricates_a_destination() {
        let work = session(2, "Work");
        for intake in [None, Some(session(1, "Intake").key)] {
            let sections = move_destination_sections(&[&work], intake, 1);
            assert_eq!(sections.len(), 1);
            assert_eq!(sections[0].label, Some("Named sessions"));
        }
        assert!(move_destination_sections(&[], None, 1).is_empty());
    }

    #[test]
    fn imported_destination_retains_its_pending_organization_identity() {
        let mut imported = session(2, "Imported");
        imported.awaiting_organization = true;
        let sections = move_destination_sections(&[&imported], None, 1);
        assert!(
            sections[0].rows[0]
                .detail
                .as_ref()
                .unwrap()
                .contains("still awaiting organization")
        );
        let sections = rank_sections(sections, "Imported");
        assert_eq!(sections[0].rows.len(), 1);
    }

    fn ranked(items: &[&'static str], query: &str) -> Vec<&'static str> {
        rank(items.to_vec(), query, |item| (*item).to_owned())
    }

    #[test]
    fn subsequences_match_when_no_substring_does() {
        assert_eq!(
            ranked(&["Close Pane", "Split Right", "Settings"], "spr"),
            vec!["Split Right"]
        );
    }

    #[test]
    fn word_starts_outrank_inner_hits_and_every_token_must_match() {
        assert_eq!(
            ranked(&["Unsplit", "Split Down", "Split Right"], "split"),
            vec!["Split Down", "Split Right", "Unsplit"]
        );
        assert_eq!(
            ranked(&["Split Down", "Split Right"], "split ri"),
            vec!["Split Right"]
        );
        assert!(ranked(&["Zoom"], "qq").is_empty());
    }
}
