//! Palettes, the path picker, dialogs, and the find bar. Lists are the Kit
//! [`Command`] palette; text entry is Kit [`Input`]. Every modal sits on a
//! backdrop that closes it on click, and its control takes the keyboard on open.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Context, Entity, EntityId, IntoElement, MouseButton,
    ParentElement, SharedString, Styled, Subscription, WeakEntity, Window, div, px,
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
use super::skin::{Look, Surface, Surfaced};

mod goto_facts;
mod goto_filter;
mod goto_frecency;
mod goto_search;
pub(crate) mod launcher;

pub(crate) use goto_filter::GotoPalette;
pub(crate) use goto_frecency::Store as GotoFrecency;
pub(crate) use goto_search::GotoSearch;

#[cfg(test)]
mod palette_tests;
#[cfg(test)]
mod window_tests;

/// Transient disclosure state, reset whenever a palette opens.
#[derive(Default)]
pub(super) struct PaletteView {
    all_commands: bool,
    details: bool,
}

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
        | Modal::Windows
        | Modal::MoveResources
        | Modal::Path => {
            desk.palette_view = PaletteView::default();
            let state = palette_state(desk, window, cx);
            state.update(cx, |state, cx| {
                state.set_loading(false, window, cx);
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
        }
        Modal::Settings => {
            desk.settings_page = None;
            let state = palette_state(desk, window, cx);
            state.update(cx, |state, cx| {
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
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
                self.run_search(value, case, cx);
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
    /// resize, focus, acknowledge a terminal, or record a visit.
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
        prepare_goto(self, &query);
        let index = state.read(cx).selected_index();
        let selected = index.and_then(|index| {
            self.goto_search
                .ranked()
                .get(index.section)
                .and_then(|section| section.rows.get(index.row))
                .and_then(goto_id)
        });
        let entries = self.goto_search.candidates();
        self.goto.selected = selected;
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
    /// A Go To pane or tab is remembered only when that destination is still listed.
    fn pick(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.note_goto(&target);
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
            Target::Window { handle, desk, tab } => {
                self.close_modal(window, cx);
                if desk.entity_id() == cx.entity_id() {
                    if let Some(tab) = tab {
                        self.select_tab(&tab);
                    }
                    self.after_command(window, cx);
                    return;
                }
                self.after_command(window, cx);
                // Another window can be updated once this window's lease ends.
                cx.defer(move |cx| raise_window(handle, &desk, tab.as_deref(), cx));
            }
            Target::Work(destination) => {
                self.close_modal(window, cx);
                self.navigate_work(destination, cx);
                self.after_command(window, cx);
            }
            Target::Setting(page) => {
                self.modal = Modal::Settings;
                self.settings_page = Some(page);
                self.mark_prepared();
                super::settings_panel::prepare(self, window, cx);
                window.focus(&self.focus, cx);
                cx.notify();
            }
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

    /// Record a Go To open. A missing pane or tab, and every other row, is ignored.
    fn note_goto(&mut self, target: &Target) {
        let now = goto_frecency::now();
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let boot = link.connection.server_id();
        let key = match target {
            Target::Pane(id) => {
                let listed = link.connection.pane(id).is_some()
                    || link.workspace.tabs.iter().any(|tab| {
                        crate::workspace::layout::placements(&tab.root)
                            .iter()
                            .any(|placement| placement.terminal_id.as_ref() == Some(id))
                    });
                let spelling = id.to_string();
                goto_frecency::validated_key(
                    boot.as_deref(),
                    goto_frecency::Choice::Pane {
                        id: &spelling,
                        listed,
                    },
                )
            }
            Target::Tab(id) => {
                let listed = link.workspace.tabs.iter().any(|tab| tab.id == *id);
                goto_frecency::validated_key(
                    boot.as_deref(),
                    goto_frecency::Choice::Tab { id, listed },
                )
            }
            Target::Command
            | Target::Session(_)
            | Target::Window { .. }
            | Target::Work(_)
            | Target::Setting(_)
            | Target::Move(_, _)
            | Target::Path(_) => {
                goto_frecency::validated_key(boot.as_deref(), goto_frecency::Choice::Other)
            }
        };
        self.goto_frecency.note(key, now);
    }
}

/// What a palette row does when confirmed.
#[derive(Clone)]
enum Target {
    Command,
    Pane(ResourceId),
    Tab(String),
    Session(String),
    /// An open native window, and the tab to show there. Never opens one.
    Window {
        handle: AnyWindowHandle,
        desk: WeakEntity<Desk>,
        tab: Option<String>,
    },
    Work(super::fleet::Destination),
    Setting(super::settings_panel::Page),
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
    let look = desk.look();
    let card = match desk.modal.clone() {
        Modal::None => return div().into_any_element(),
        Modal::Commands
        | Modal::Goto
        | Modal::WindowSession
        | Modal::Windows
        | Modal::MoveResources => launcher::palette(desk, colors, window, cx),
        Modal::Path => path_panel(desk, colors, &look, window, cx),
        Modal::Settings if desk.settings_page.is_some() => {
            super::settings_panel::panel(desk, colors, window, cx)
        }
        Modal::Settings => launcher::palette(desk, colors, window, cx),
        Modal::Notifications => notifications_panel(desk, colors, &look, cx),
        Modal::NativeLaunch => super::native_launch::panel(desk, colors, &look, cx),
        Modal::Rename { .. } => rename_dialog(desk, colors, &look, cx),
        Modal::Session { create, .. } => session_dialog(desk, create, colors, &look, cx),
        Modal::KillSession { name, .. } => confirm(
            format!("Kill {name}?"),
            "This ends every process in the session, for every client, and deletes it. Closing a pane, tab, or window never does this.",
            "Kill Session",
            colors,
            &look,
            cx,
            |desk, window, cx| desk.confirm_kill_session(window, cx),
        ),
        Modal::AddHost => add_host_dialog(desk, colors, &look, cx),
        Modal::RemoveHost { name } => confirm(
            format!("Remove {name}?"),
            "This forgets the machine here. Its server and terminals keep running; `phux host add` registers it again.",
            "Remove Host",
            colors,
            &look,
            cx,
            |desk, window, cx| desk.confirm_remove_host(window, cx),
        ),
        Modal::Terminate { title, .. } => confirm(
            format!("Terminate {title}?"),
            "This ends the process for every view and every client attached to it. Closing a pane only detaches.",
            "Terminate",
            colors,
            &look,
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

/// A dialog or palette card. The skin decides how much it lifts; the backing
/// stays near-opaque so text reads over terminal content.
pub(crate) fn card(look: &Look, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .text_color(look.ink)
        .surface(look, Surface::Popover)
        .occlude()
}

/// `glass` points of corner on an inner element, scaled to the skin's kit
/// radius so the glass skin keeps its shipped corners.
pub(crate) fn corner(look: &Look, glass: f32) -> gpui::Pixels {
    let kit = crate::settings::skin::GLASS.shape.kit_radius;
    px(glass * look.skin.shape.kit_radius / kit)
}

/// An inset row inside a [`card`]: its corner follows the card's.
fn inset_radius(look: &Look) -> gpui::Pixels {
    px((look.skin.shape.card_radius - 4.).max(0.))
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
    let look = desk.look();
    card(&look, 420.)
        .absolute()
        .top(px(12.))
        .right(px(12.))
        .p(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            div().flex_grow(1.).child(
                Input::new(input)
                    .appearance(false)
                    .prefix(Icon::new(IconName::Search).small()),
            ),
        )
        .child(
            div()
                .px(px(6.))
                .py(px(3.))
                .rounded(corner(&look, 5.))
                .bg(from_hex(&colors.theme.foreground).opacity(0.045))
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
                    desk.run_search(query, case, cx);
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
#[derive(Clone)]
struct Section {
    label: Option<&'static str>,
    rows: Vec<Row>,
}

#[derive(Clone)]
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
    /// Passive metadata card. Absent for commands and for a row with nothing
    /// to show. Highlighting reads this; it does not open the terminal.
    preview: Option<goto_facts::FactsCard>,
}

fn simple_row(title: &str, detail: String, category: &'static str, target: Target) -> Row {
    Row {
        title: title.to_owned().into(),
        keywords: detail.clone(),
        detail: Some(detail.into()),
        target,
        command: None,
        category,
        chord: None,
        agent: None,
        preview: None,
    }
}

fn settings_sections() -> Vec<Section> {
    vec![Section {
        label: Some("Settings"),
        rows: super::settings_panel::Page::ALL
            .into_iter()
            .map(|page| {
                let mut row = simple_row(
                    page.title(),
                    page.description().to_owned(),
                    "Settings",
                    Target::Setting(page),
                );
                row.keywords = page.keywords().to_owned();
                row
            })
            .collect(),
    }]
}

fn work_rows(desk: &Desk) -> Vec<Row> {
    desk.work_catalog()
        .into_iter()
        .map(|work| {
            let destination = work.destination;
            let status = if work.online { "" } else { " · offline" };
            let intake = if work.intake { " · Intake" } else { "" };
            let detail = format!(
                "{} · {} terminals{intake}{status}",
                destination.host_label(),
                work.terminals
            );
            let mut row = simple_row(
                &destination.name.clone(),
                detail,
                "Work",
                Target::Work(destination),
            );
            if let Some((id, name)) = work.project {
                row.keywords.push_str(&format!(" {id} {name}"));
            }
            row
        })
        .collect()
}

fn command_sections(desk: &Desk, cx: &App) -> Vec<Section> {
    const ESSENTIALS: &[&str] = &["new", "native-agent", "split-right", "folder", "find"];
    let organizing = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.organization());
    let monitoring = desk.desktop.read(cx).performance_is_open();
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
            chord: desk.shown_chord(command.id),
            agent: None,
            preview: None,
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

/// A small launch list on open; typing always searches the complete catalogue.
fn command_results(desk: &Desk, cx: &App, query: &str) -> Vec<Section> {
    let mut sections = command_sections(desk, cx);
    if !query.trim().is_empty() {
        sections.extend(settings_sections());
        return rank_sections(sections, query);
    }
    if !desk.palette_view.all_commands {
        sections.retain(|section| section.label == Some("Essentials"));
    }
    for section in &mut sections {
        section.label = None;
    }
    vec![Section {
        label: None,
        rows: sections
            .into_iter()
            .flat_map(|section| section.rows)
            .collect(),
    }]
}

fn goto_sections(desk: &Desk) -> Vec<Section> {
    let Some(link) = desk.link.as_ref() else {
        return vec![Section {
            label: Some("Work"),
            rows: work_rows(desk),
        }];
    };
    let mut agents = Vec::new();
    let mut terminals = Vec::new();
    for pane in link.connection.panes() {
        let agent = link.connection.agents.get(&pane.terminal_id);
        let title = desk.pane_title(Some(&pane.terminal_id));
        let directory = desk::short_path(pane.cwd.clone());
        let detail = format!("{} · {directory}", pane.session_name);
        let card_title = agent
            .map(|info| info.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| title.clone());
        let preview = goto_facts::facts_card(Some(&goto_facts::GotoSubject::Pane {
            title: card_title,
            agent: agent.cloned(),
            session: pane.session_name.clone(),
            directory,
            tab: tab_label(desk, &pane.terminal_id),
        }));
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
            preview,
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
            let placed = crate::workspace::layout::placements(&tab.root);
            if placed.len() < 2 {
                return None;
            }
            let detail = format!("Tab {} · {} panes", index + 1, placed.len());
            let title = desk::tab_title(desk, tab);
            let panes = placed
                .into_iter()
                .filter_map(|placement| tab_pane(desk, placement))
                .collect();
            Some(Row {
                title: title.clone().into(),
                keywords: detail.clone(),
                detail: Some(detail.into()),
                target: Target::Tab(tab.id.clone()),
                command: None,
                category: "Tabs",
                chord: None,
                agent: None,
                preview: goto_facts::facts_card(Some(&goto_facts::GotoSubject::Tab {
                    title,
                    panes,
                })),
            })
        })
        .collect::<Vec<_>>();
    [
        ("Work", work_rows(desk)),
        ("Agents", agents),
        ("Terminals", terminals),
        ("Tabs", tabs),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .map(|(label, rows)| Section {
        label: Some(label),
        rows,
    })
    .collect()
}

/// What one open window shows, read for the window overview.
struct Shown {
    session: String,
    host: Option<String>,
    tabs: Vec<(String, String)>,
    active: Option<String>,
}

impl Shown {
    fn of(desk: &Desk) -> Self {
        let (tabs, active) = desk.link.as_ref().map_or((Vec::new(), None), |link| {
            let tabs = link
                .workspace
                .tabs
                .iter()
                .map(|tab| (tab.id.clone(), desk::tab_title(desk, tab)))
                .collect();
            (tabs, Some(link.workspace.active_id.clone()))
        });
        Self {
            session: if desk.session.is_empty() {
                "No session".to_owned()
            } else {
                desk.session.clone()
            },
            host: desk.remote.clone(),
            tabs,
            active,
        }
    }

    fn host_label(&self) -> &str {
        self.host.as_deref().unwrap_or("Local")
    }
}

/// Section label of the window overview, whose tab rows sit under their window.
pub(super) const WINDOWS: &str = "Windows";

/// One row per open native window, each followed by its tabs. Windows are
/// numbered in the order they opened; the session and host are phux's. The
/// window this list is in is Current, and so is the tab it shows. Another
/// window's tab on screen is Shown.
fn window_sections(desk: &Desk, current: EntityId, cx: &App) -> Vec<Section> {
    let mut rows = Vec::new();
    for (index, (handle, entity, quick)) in desk.desktop.read(cx).desk_windows().enumerate() {
        let here = entity.entity_id() == current;
        let shown = if here {
            Shown::of(desk)
        } else {
            Shown::of(entity.read(cx))
        };
        let name = if quick {
            "Quick Terminal".to_owned()
        } else {
            format!("Window {}", index + 1)
        };
        let count = match shown.tabs.len() {
            0 => "no tabs".to_owned(),
            1 => "1 tab".to_owned(),
            count => format!("{count} tabs"),
        };
        let mark = if here { "Current · " } else { "" };
        let target = |tab: Option<String>| Target::Window {
            handle,
            desk: entity.downgrade(),
            tab,
        };
        rows.push(simple_row(
            &name,
            format!("{mark}{} · {} · {count}", shown.session, shown.host_label()),
            "Windows",
            target(None),
        ));
        let total = shown.tabs.len();
        for (position, (id, title)) in shown.tabs.iter().enumerate() {
            let mark = match (shown.active.as_deref() == Some(id.as_str()), here) {
                (true, true) => "Current · ",
                (true, false) => "Shown · ",
                (false, _) => "",
            };
            let host = shown
                .host
                .as_deref()
                .map(|host| format!(" · {host}"))
                .unwrap_or_default();
            rows.push(simple_row(
                title,
                format!(
                    "{mark}Tab {} of {total} · {name} · {}{host}",
                    position + 1,
                    shown.session
                ),
                "Tabs",
                target(Some(id.clone())),
            ));
        }
    }
    if rows.is_empty() {
        return Vec::new();
    }
    vec![Section {
        label: Some(WINDOWS),
        rows,
    }]
}

/// Bring an open window forward, out of the Dock when minimized, and show
/// `tab` there. A window closed since it was listed is left closed.
fn raise_window(handle: AnyWindowHandle, desk: &WeakEntity<Desk>, tab: Option<&str>, cx: &mut App) {
    let activate = crate::diag::may_activate();
    if activate {
        cx.activate(true);
    }
    let _ = handle.update(cx, |_, window, cx| {
        if activate {
            window.activate_window();
        }
        let _ = desk.update(cx, |desk, cx| {
            if let Some(tab) = tab {
                desk.select_tab(tab);
            }
            desk.after_command(window, cx);
        });
    });
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
                preview: None,
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
        " · imported"
    } else {
        ""
    };
    Row {
        title: title.into(),
        detail: Some(format!("{count} selected{imported}").into()),
        keywords: session.name.clone(),
        target: Target::Move(session.key, name),
        command: None,
        category: "Organization",
        chord: None,
        agent: None,
        preview: None,
    }
}

/// The tab that shows `terminal`, by the same title the tab row uses.
fn tab_label(desk: &Desk, terminal: &ResourceId) -> Option<String> {
    let link = desk.link.as_ref()?;
    let tab = link.workspace.tabs.iter().find(|tab| {
        crate::workspace::layout::placements(&tab.root)
            .iter()
            .any(|placement| placement.terminal_id.as_ref() == Some(terminal))
    })?;
    Some(desk::tab_title(desk, tab))
}

/// One pane on a tab card: the agent name and its state word, or the terminal
/// title and its directory. Layout metadata only.
fn tab_pane(
    desk: &Desk,
    placement: &crate::workspace::layout::Placement,
) -> Option<goto_facts::Fact> {
    let id = placement.terminal_id.as_ref()?;
    let link = desk.link.as_ref()?;
    let pane = link.connection.pane(id)?;
    let agent = link.connection.agents.get(id);
    let name = agent
        .map(|info| info.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| desk.pane_title(Some(id)));
    let detail = agent
        .and_then(|info| {
            let word = info.word();
            (!word.is_empty()).then(|| word.to_owned())
        })
        .unwrap_or_else(|| desk::short_path(pane.cwd.clone()));
    Some(goto_facts::Fact::new(name, detail))
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
        Target::Work(destination) => Some(goto_filter::GotoId::Work(destination.clone())),
        Target::Command => row.command.map(goto_filter::GotoId::Command),
        Target::Setting(page) => Some(goto_filter::GotoId::Setting(*page)),
        Target::Pane(id) => Some(goto_filter::GotoId::Pane(id.clone())),
        Target::Tab(id) => Some(goto_filter::GotoId::Tab(id.clone())),
        _ => None,
    }
}

#[cfg(test)]
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
    goto_search::rank_sections(sections, query)
}

/// Fold Go To rows when their metadata revision changes, then rank the query.
fn prepare_goto(desk: &mut Desk, query: &str) {
    let revision = search_revision(desk);
    let filter = desk.goto.filter;
    if desk.goto_search.needs_rebuild(revision) {
        let sections = goto_sections(desk);
        desk.goto_search.rebuild(revision, sections);
    }
    let generation = desk.goto_frecency.generation();
    if desk.goto_search.rank_current(query, filter, generation) {
        return;
    }
    let boosts = desk.goto_frecency.boosts(goto_frecency::now());
    let boot = desk
        .link
        .as_ref()
        .and_then(|link| link.connection.server_id());
    desk.goto_search
        .ensure_ranked(query, filter, generation, |row| {
            goto_boost(&boosts, boot.as_deref(), row)
        });
}

fn goto_boost(
    boosts: &std::collections::HashMap<goto_frecency::Key, i64>,
    boot: Option<&str>,
    row: &Row,
) -> i64 {
    let Some(boot) = boot else {
        return 0;
    };
    let key = match &row.target {
        Target::Pane(id) => goto_frecency::Key::pane(boot, &id.to_string()),
        Target::Tab(id) => goto_frecency::Key::tab(boot, id),
        _ => None,
    };
    key.and_then(|key| boosts.get(&key).copied()).unwrap_or(0)
}

/// Connection drains cover panes and agents. Tabs are local, so they mix in too.
fn search_revision(desk: &Desk) -> goto_search::SearchRevision {
    let Some(link) = desk.link.as_ref() else {
        return goto_search::SearchRevision {
            connection: desk.fleet.revision.rotate_left(32),
            workspace: 0,
        };
    };
    goto_search::SearchRevision {
        connection: link
            .connection
            .revision()
            .wrapping_add(desk.fleet.revision.rotate_left(32)),
        workspace: workspace_stamp(&link.workspace.tabs),
    }
}

fn workspace_stamp(tabs: &[crate::workspace::layout::DeskTab]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for tab in tabs {
        hash = mix_bytes(hash, tab.id.as_bytes());
        hash = mix_bytes(hash, tab.focused_id.as_bytes());
        hash = mix_bytes(hash, tab.title.as_deref().unwrap_or("").as_bytes());
        hash = mix_node(hash, &tab.root);
    }
    hash
}

fn mix_node(mut hash: u64, node: &crate::workspace::layout::LayoutNode) -> u64 {
    match node {
        crate::workspace::layout::LayoutNode::Leaf { placement } => {
            hash = mix_bytes(hash, placement.id.as_bytes());
            mix_bytes(hash, placement.terminal_label.as_bytes())
        }
        crate::workspace::layout::LayoutNode::Split {
            id, first, second, ..
        } => {
            hash = mix_bytes(hash, id.as_bytes());
            hash = mix_node(hash, first);
            mix_node(hash, second)
        }
    }
}

fn mix_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    hash ^= bytes.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn query_terms(query: &str) -> Vec<String> {
    goto_search::PreparedQuery::parse(query).into_terms()
}

fn path_panel(
    desk: &mut Desk,
    colors: &Palette,
    look: &Look,
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
                .group("path-row")
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
                                .invisible()
                                .group_hover("path-row", |style| style.visible())
                                .label("Open terminal")
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
                .py(px(36.))
                .px(px(24.))
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.))
                .text_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::Search).text_color(cx.theme().muted_foreground))
                .child(if pending { "Searching…" } else { "No paths." })
        })
        .footer(move |_, _, _| {
            div()
                .px(px(12.))
                .py(px(10.))
                .flex()
                .justify_between()
                .gap(px(12.))
                .text_xs()
                .text_color(muted)
                .child(div().min_w_0().truncate().child(message.clone()))
                .child(div().flex_shrink_0().child("Tab: open terminal"))
        })
        .on_query(move |query, window, cx| {
            let _ = desk_handle.update(cx, |desk, cx| desk.palette_query(query, window, cx));
        })
        .on_confirm(move |index: IndexPath, window, cx| {
            if let Some(target) = targets.get(index.row).cloned() {
                let _ = confirm_handle.update(cx, |desk, cx| desk.pick(target, window, cx));
            }
        });
    card(look, 640.)
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
    look: &Look,
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
                .px(px(12.))
                .py(px(12.))
                .rounded(inset_radius(look))
                .border_1()
                .border_color(from_hex(&colors.theme.foreground).opacity(0.055))
                .bg(from_hex(&colors.theme.foreground).opacity(if entry.read {
                    0.015
                } else {
                    0.04
                }))
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
        .pt(px(14.))
        .pb(px(8.))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(from_hex(&colors.muted))
        .child(format!("{title} · {unread} unread alerts"))
        .into_any_element()
}

fn notifications_panel(
    desk: &Desk,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let agent_rows = notification_rows(desk, colors, look, cx, true);
    let general_rows = notification_rows(desk, colors, look, cx, false);
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
                .px(px(12.))
                .py(px(16.))
                .rounded(inset_radius(look))
                .bg(from_hex(&colors.theme.foreground).opacity(0.025))
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
    card(look, 460.)
        .mt(px(60.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .px(px(8.))
                .py(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(from_hex(&colors.theme.foreground))
                        .child("Notifications"),
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
        .child(if empty {
            div()
                .px(px(20.))
                .py(px(28.))
                .rounded(inset_radius(look))
                .border_1()
                .border_color(from_hex(&colors.theme.foreground).opacity(0.06))
                .bg(from_hex(&colors.theme.foreground).opacity(0.025))
                .text_sm()
                .text_color(from_hex(&colors.muted))
                .child("No notifications")
                .into_any_element()
        } else {
            div()
                .id("notification-list")
                .max_h(px(480.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(6.))
                .children(rows)
                .into_any_element()
        })
        .into_any_element()
}

fn dialog_card(title: &str, colors: &Palette, look: &Look) -> gpui::Div {
    card(look, 440.)
        .mt(px(120.))
        .p(px(24.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(
            div()
                .pb(px(4.))
                .text_size(px(18.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(from_hex(&colors.theme.foreground))
                .child(title.to_owned()),
        )
}

fn dialog_buttons(confirm: Button, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .pt(px(8.))
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

fn rename_dialog(desk: &Desk, colors: &Palette, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let Some(input) = desk.input_entity(InputSlot::Rename) else {
        return div().into_any_element();
    };
    dialog_card("Rename Tab", colors, look)
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
    look: &Look,
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
        look,
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

fn add_host_dialog(
    desk: &Desk,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let (Some(target), Some(name)) = (
        desk.input_entity(InputSlot::HostTarget),
        desk.input_entity(InputSlot::HostName),
    ) else {
        return div().into_any_element();
    };
    dialog_card("Add Host", colors, look)
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
    look: &Look,
    cx: &mut Context<Desk>,
    run: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
) -> AnyElement {
    dialog_card(&title, colors, look)
        .border_color(from_hex(&colors.theme.danger).opacity(0.55))
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
    fn work_and_settings_rows_preserve_actual_indices_during_filter_realignment() {
        let work = crate::app::fleet::Destination {
            host: Some("mini".into()),
            authority: None,
            key: Some(SessionKey::new([3; 16]).unwrap()),
            name: "repo".into(),
        };
        let target = phux_protocol::ResourceId::local(7);
        let rows = vec![
            super::simple_row("repo", String::new(), "Work", Target::Work(work)),
            super::simple_row(
                "Appearance",
                String::new(),
                "Settings",
                Target::Setting(crate::app::settings_panel::Page::Appearance),
            ),
            super::simple_row(
                "agent",
                String::new(),
                "Agents",
                Target::Pane(target.clone()),
            ),
        ];
        let sections = vec![Section { label: None, rows }];
        let ids = section_ids(&sections);
        assert_eq!(ids[0].len(), 3);
        assert_eq!(
            selection_path(&ids, Some(&GotoId::Pane(target))),
            Some((0, 2))
        );
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
                preview: None,
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
            preview: None,
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

    #[test]
    fn a_filter_keeps_the_highlighted_rows_facts_and_drops_a_hidden_card() {
        use super::goto_facts::{GotoSubject, facts_card};
        use crate::connection::AgentState;

        let card = facts_card(Some(&GotoSubject::Pane {
            title: "Worker".into(),
            agent: Some(agent(AgentState::Working)),
            session: "work".into(),
            directory: "~/repo".into(),
            tab: None,
        }))
        .expect("highlighted worker has a card");
        let mut worker = goto_row("Worker", Some(AgentState::Working), 2);
        worker.preview = Some(card.clone());
        let sections = vec![Section {
            label: Some("Agents"),
            rows: vec![worker, goto_row("Idle", Some(AgentState::Idle), 3)],
        }];
        let ranked = rank_sections(filter_sections(sections, GotoFilter::Working), "work");
        let shown = ranked[0].rows[0]
            .preview
            .as_ref()
            .expect("card follows the row");
        assert_eq!(shown, &card);
        assert_eq!(
            shown
                .facts
                .iter()
                .map(|fact| (fact.name.as_str(), fact.value.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("Status", "working"),
                ("Directory", "~/repo"),
                ("Session", "work"),
            ]
        );

        let hidden = rank_sections(
            filter_sections(
                vec![Section {
                    label: Some("Agents"),
                    rows: vec![goto_row("Idle", Some(AgentState::Idle), 3)],
                }],
                GotoFilter::Working,
            ),
            "",
        );
        assert!(hidden.is_empty(), "a hidden row takes its card with it");
        assert!(facts_card(None).is_none());
    }
}
