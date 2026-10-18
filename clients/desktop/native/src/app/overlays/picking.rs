//! What a palette row does: the query routing, the Go To filter and
//! highlight, and running the confirmed row.

use gpui::{AnyWindowHandle, Context, Entity, WeakEntity, Window};
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::CommandState;
use phux_protocol::ResourceId;
use phux_protocol::ids::SessionKey;

use super::{
    PathItem, Section, goto_filter, goto_frecency, goto_id, prepare_goto, raise_window, section_ids,
};
use crate::app::desk::{Desk, Modal};

impl Desk {
    /// The palette query changed. In Go To, a leading `>` switches to commands
    /// and keeps the rest of the query; the path picker searches the host.
    pub(super) fn palette_query(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.modal {
            Modal::Commands | Modal::Goto if query.starts_with('!') => {
                let text = query[1..].trim_start();
                if !text.is_empty() {
                    self.capture.draft = text.to_owned();
                }
                self.open_modal(Modal::Capture, window, cx);
            }
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
    pub(super) fn align_goto_highlight(
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

    /// Open Settings directly on `page`, replacing any open modal.
    pub(crate) fn show_settings_page(
        &mut self,
        page: crate::app::settings_panel::Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = Modal::Settings;
        self.settings_page = Some(page);
        self.mark_prepared();
        crate::app::settings_panel::prepare(self, window, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Run a confirmed palette row. Commands already ran through their [`Run`] action.
    /// A Go To pane or tab is remembered only when that destination is still listed.
    pub(super) fn pick(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
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
            Target::Setting(page) => self.show_settings_page(page, window, cx),
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
    pub(super) fn note_goto(&mut self, target: &Target) {
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
            _ => None,
        };
        self.goto_frecency.note(key, now);
    }
}

/// What a palette row does when confirmed.
#[derive(Clone)]
pub(super) enum Target {
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
    Work(crate::app::fleet::Destination),
    Setting(crate::app::settings_panel::Page),
    Move(SessionKey, String),
    Path(PathItem),
}
