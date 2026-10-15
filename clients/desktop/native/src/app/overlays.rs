//! Palettes, the path picker, dialogs, and the find bar. Lists are the Kit
//! [`Command`] palette; text entry is Kit [`Input`]. Every modal sits on a
//! backdrop that closes it on click, and its control takes the keyboard on open.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, MouseButton, ParentElement, SharedString,
    Styled, Subscription, Window, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, IndexPath, Sizable};
use phux_protocol::ResourceId;
use phux_protocol::ids::SessionKey;

use crate::connection::AgentInfo;
use crate::connection::path::{self as paths};
use crate::settings::theme::Palette;

use super::color::from_hex;
use super::desk::{self, Desk, Modal};

mod goto_filter;
mod launcher;

pub(crate) use goto_filter::GotoPalette;

#[cfg(test)]
mod palette_tests;

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
                state.set_loading(false, window, cx);
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

    /// Narrow Go To by agent state. The typed query stays, and the highlight
    /// follows the same entry when it is still listed. This does not open,
    /// resize, focus, or acknowledge a terminal.
    pub(crate) fn set_goto_filter(
        &mut self,
        filter: goto_filter::GotoFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Modal::Goto || self.goto.filter == filter {
            return;
        }
        let Some(state) = self.palette_state.clone() else {
            return;
        };
        let query = state.read(cx).query(cx).to_string();
        let unfiltered = goto_sections(self);
        let entries = goto_candidates(&unfiltered);
        let visible = rank_sections(filter_sections(unfiltered, self.goto.filter), &query);
        let index = state.read(cx).selected_index();
        self.goto.selected = index.and_then(|index| {
            visible
                .get(index.section)
                .and_then(|section| section.rows.get(index.row))
                .and_then(goto_id)
        });
        let mut cursor = goto_filter::GotoCursor {
            query,
            selected: self.goto.selected.clone(),
        };
        goto_filter::apply_filter(&mut cursor, &entries, filter);
        self.goto.filter = filter;
        self.goto.selected = cursor.selected;
        self.goto.align = true;
        state.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// Move the Go To highlight onto the entry a filter change kept.
    fn align_goto_highlight(
        &mut self,
        sections: &[Section],
        state: &Entity<CommandState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Modal::Goto || !self.goto.align {
            return;
        }
        let ids = section_ids(sections);
        let path = goto_filter::selection_path(&ids, self.goto.selected.as_ref());
        self.goto.selected = path.and_then(|(section, row)| ids.get(section)?.get(row).cloned());
        let desired = path.map(|(section, row)| IndexPath::new(row).section(section));
        if state.read(cx).selected_index() == desired {
            self.goto.align = false;
            return;
        }
        let state = state.clone();
        let desk = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let applied = state.update(cx, |state, cx| {
                state.set_selected_index(desired, window, cx);
                state.selected_index() == desired
            });
            if applied {
                let _ = desk.update(cx, |desk, _| desk.goto.align = false);
            }
        });
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
            launcher::palette(desk, colors, window, cx)
        }
        Modal::Path => path_panel(desk, colors, window, cx),
        Modal::Settings => super::settings_panel::panel(desk, colors, window, cx),
        Modal::Notifications => notifications_panel(desk, colors, cx),
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
        .size_full()
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
    title: SharedString,
    detail: Option<SharedString>,
    /// Extra searchable text, not shown.
    keywords: String,
    target: Target,
    command: Option<&'static str>,
    category: &'static str,
    chord: Option<String>,
    /// Agent badge, when this row is one. State chips read its predicates.
    agent: Option<AgentInfo>,
}

fn command_sections(desk: &Desk, cx: &App) -> Vec<Section> {
    const ESSENTIALS: &[&str] = &["new", "split-right", "folder", "settings"];
    let organizing = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.organization());
    let monitoring = desk.desktop.read(cx).performance_is_open();
    let chords = crate::settings::shown_chords(desk.keymap(), &crate::settings::default_chords());
    let mut sections: Vec<Section> = Vec::new();
    for command in crate::settings::builtin_commands()
        .iter()
        .filter(|command| {
            command.is_listed()
                && !matches!(command.id, "palette" | "goto")
                && (organizing || !command.needs_organization())
                && (monitoring || command.id != "performance-monitor-stop")
        })
    {
        let mut keywords = String::from(command.id);
        for word in command.keywords {
            keywords.push(' ');
            keywords.push_str(word);
        }
        let row = Row {
            title: command.title.into(),
            detail: Some(command.description.into()),
            keywords,
            target: Target::Command,
            command: Some(command.id),
            category: command.group,
            chord: chords
                .iter()
                .find(|(id, _)| id == command.id)
                .map(|(_, chord)| crate::settings::display_chord(chord)),
            agent: None,
        };
        let label = if ESSENTIALS.contains(&command.id) {
            "Essentials"
        } else {
            command.group
        };
        if let Some(section) = sections
            .iter_mut()
            .find(|section| section.label == Some(label))
        {
            section.rows.push(row);
        } else {
            sections.push(Section {
                label: Some(label),
                rows: vec![row],
            });
        }
    }
    if let Some(essentials) = sections
        .iter_mut()
        .find(|section| section.label == Some("Essentials"))
    {
        essentials
            .rows
            .sort_by_key(|row| ESSENTIALS.iter().position(|id| Some(*id) == row.command));
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
                .unwrap_or(title)
                .into(),
            keywords: agent
                .map(|info| format!("{detail} {}", info.word()))
                .unwrap_or_else(|| detail.clone()),
            detail: Some(
                (match agent.map(|info| info.word()) {
                    Some(word) if !word.is_empty() => format!("{word} · {detail}"),
                    _ => detail,
                })
                .into(),
            ),
            target: Target::Pane(pane.terminal_id.clone()),
            command: None,
            category: if agent.is_some() {
                "Agents"
            } else {
                "Terminals"
            },
            chord: None,
            agent: agent.cloned(),
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
                title: desk::tab_title(desk, tab).into(),
                keywords: detail.clone(),
                detail: Some(detail.into()),
                target: Target::Tab(tab.id.clone()),
                command: None,
                category: "Tabs",
                chord: None,
                agent: None,
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
                title: session.name.clone().into(),
                keywords: detail.clone(),
                detail: Some(detail.into()),
                target: Target::Session(session.name.clone()),
                command: None,
                category: "Sessions",
                chord: None,
                agent: None,
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
        title: title.into(),
        detail: Some(format!("Move {count} selected · execution unchanged{imported}").into()),
        keywords: session.name.clone(),
        target: Target::Move(session.key, name),
        command: None,
        category: "Organization",
        chord: None,
        agent: None,
    }
}

fn filter_sections(mut sections: Vec<Section>, filter: goto_filter::GotoFilter) -> Vec<Section> {
    if filter == goto_filter::GotoFilter::All {
        return sections;
    }
    for section in &mut sections {
        section
            .rows
            .retain(|row| filter.matches(row.agent.as_ref()));
    }
    sections.retain(|section| !section.rows.is_empty());
    sections
}

fn goto_id(row: &Row) -> Option<goto_filter::GotoId> {
    match &row.target {
        Target::Pane(id) => Some(goto_filter::GotoId::Pane(id.clone())),
        Target::Tab(id) => Some(goto_filter::GotoId::Tab(id.clone())),
        _ => None,
    }
}

fn goto_candidates(sections: &[Section]) -> Vec<goto_filter::GotoCandidate> {
    sections
        .iter()
        .flat_map(|section| section.rows.iter())
        .filter_map(|row| {
            Some(goto_filter::GotoCandidate {
                id: goto_id(row)?,
                agent: row.agent.clone(),
            })
        })
        .collect()
}

fn section_ids(sections: &[Section]) -> Vec<Vec<goto_filter::GotoId>> {
    sections
        .iter()
        .map(|section| section.rows.iter().filter_map(goto_id).collect())
        .collect()
}

/// Rank every row against `query` as one flat list; an empty query keeps the sections.
fn rank_sections(sections: Vec<Section>, query: &str) -> Vec<Section> {
    let query = query.trim();
    if query.is_empty() {
        return sections;
    }
    let folded = query.to_lowercase();
    let tokens: Vec<&str> = folded.split_whitespace().collect();
    let mut ranked: Vec<(i64, Row)> = sections
        .into_iter()
        .flat_map(|section| section.rows)
        .filter_map(|row| {
            let title = row.title.to_lowercase();
            let metadata = format!(
                "{} {} {}",
                row.category,
                row.detail.as_deref().unwrap_or_default(),
                row.keywords,
            )
            .to_lowercase();
            let score = tokens.iter().try_fold(0, |total, token| {
                let title_score = literal_score(&title, token)
                    .map(|score| score * 4)
                    .or_else(|| subsequence_score(&title, token));
                // Long descriptions are useful search terms, not a fuzzy-match canvas.
                let metadata_score = literal_score(&metadata, token);
                title_score
                    .into_iter()
                    .chain(metadata_score)
                    .max()
                    .map(|score| total + score)
            })?;
            let priority = if title == folded {
                10_000
            } else if title.starts_with(&folded) {
                2_000
            } else {
                0
            };
            Some((score + priority, row))
        })
        .collect();
    ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    vec![Section {
        label: None,
        rows: ranked.into_iter().map(|(_, row)| row).collect(),
    }]
}

fn literal_score(haystack: &str, token: &str) -> Option<i64> {
    let index = haystack.find(token)?;
    let before = haystack[..index].chars().next_back();
    let word_start = before.is_none_or(|ch| ch.is_whitespace() || "/:._-".contains(ch));
    let offset = haystack[..index].chars().count().min(60) as i64;
    Some(if word_start { 200 } else { 120 } - offset)
}

fn subsequence_score(haystack: &str, token: &str) -> Option<i64> {
    let mut chars = haystack.chars();
    let mut gaps = 0;
    for wanted in token.chars() {
        gaps += chars.position(|ch| ch == wanted)? as i64;
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
    use super::goto_filter::{GotoCursor, GotoFilter, GotoId, apply_filter, selection_path};
    use super::{
        AgentInfo, Row, Section, Target, filter_sections, goto_candidates,
        move_destination_sections, rank_sections, section_ids,
    };
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
        assert!(
            matches!(&sections[0].rows[0].target, Target::Move(key, name) if *key == intake.key && name == "Intake")
        );
        assert!(matches!(&sections[1].rows[0].target, Target::Move(key, _) if *key == work.key));
    }

    #[test]
    fn missing_or_ineligible_intake_never_fabricates_a_destination() {
        let work = session(2, "Work");
        for intake in [None, Some(session(1, "Intake").key)] {
            let sections = move_destination_sections(&[&work], intake, 1);
            assert_eq!(sections.len(), 1);
            assert!(
                matches!(&sections[0].rows[0].target, Target::Move(key, _) if *key == work.key)
            );
        }
        assert!(move_destination_sections(&[], None, 1).is_empty());
    }

    fn ranked(items: &[&'static str], query: &str) -> Vec<String> {
        let rows = items
            .iter()
            .map(|title| Row {
                title: (*title).into(),
                detail: None,
                keywords: String::new(),
                target: Target::Command,
                command: None,
                category: "",
                chord: None,
                agent: None,
            })
            .collect();
        rank_sections(vec![Section { label: None, rows }], query)
            .into_iter()
            .flat_map(|section| section.rows)
            .map(|row| row.title.to_string())
            .collect()
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

    fn agent(state: crate::connection::AgentState) -> AgentInfo {
        use crate::connection::AgentAttention;
        let attention = match state {
            crate::connection::AgentState::Blocked => AgentAttention::High,
            crate::connection::AgentState::Working => AgentAttention::Normal,
            crate::connection::AgentState::Idle => AgentAttention::None,
            crate::connection::AgentState::Done | crate::connection::AgentState::Unknown => {
                AgentAttention::Low
            }
        };
        AgentInfo {
            name: "agent".into(),
            kind: None,
            state,
            attention,
            changed_at_ms: 0,
        }
    }

    fn goto_row(title: &str, state: Option<crate::connection::AgentState>, id: u32) -> Row {
        Row {
            title: title.into(),
            detail: None,
            keywords: String::new(),
            target: Target::Pane(phux_protocol::ResourceId::local(id)),
            command: None,
            category: "Agents",
            chord: None,
            agent: state.map(agent),
        }
    }

    #[test]
    fn state_filter_then_rank_keeps_the_query_and_identity() {
        use crate::connection::AgentState;

        let sections = vec![Section {
            label: Some("Agents"),
            rows: vec![
                goto_row("Blocked", Some(AgentState::Blocked), 1),
                goto_row("Worker", Some(AgentState::Working), 2),
                goto_row("Other worker", Some(AgentState::Working), 5),
                goto_row("Idle", Some(AgentState::Idle), 3),
            ],
        }];
        let entries = goto_candidates(&sections);
        let worker = GotoId::Pane(phux_protocol::ResourceId::local(5));
        let mut cursor = GotoCursor {
            query: "other".into(),
            selected: Some(worker.clone()),
        };
        apply_filter(&mut cursor, &entries, GotoFilter::Working);
        assert_eq!(cursor.query, "other");
        assert_eq!(cursor.selected.as_ref(), Some(&worker));
        let ranked = rank_sections(
            filter_sections(sections, GotoFilter::Working),
            &cursor.query,
        );
        let ids = section_ids(&ranked);
        assert_eq!(ids, vec![vec![worker.clone()]]);
        assert_eq!(selection_path(&ids, cursor.selected.as_ref()), Some((0, 0)));

        let idle_row = || goto_row("Idle", Some(AgentState::Idle), 3);
        let idle_only = || {
            vec![Section {
                label: Some("Agents"),
                rows: vec![idle_row()],
            }]
        };
        let mut dropped = GotoCursor {
            query: "other".into(),
            selected: Some(worker),
        };
        apply_filter(
            &mut dropped,
            &goto_candidates(&idle_only()),
            GotoFilter::Idle,
        );
        assert_eq!(dropped.query, "other");
        assert!(dropped.selected.is_none());
        let hidden = rank_sections(
            filter_sections(idle_only(), GotoFilter::Idle),
            &dropped.query,
        );
        assert_eq!(
            selection_path(&section_ids(&hidden), dropped.selected.as_ref()),
            None,
            "a query that matches nothing leaves no row to highlight"
        );
        let shown = rank_sections(filter_sections(idle_only(), GotoFilter::Idle), "");
        assert_eq!(
            selection_path(&section_ids(&shown), dropped.selected.as_ref()),
            Some((0, 0))
        );
        assert!(matches!(
            &shown[0].rows[0].target,
            Target::Pane(id) if *id == phux_protocol::ResourceId::local(3)
        ));
    }
}
