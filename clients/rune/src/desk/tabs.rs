//! Tabs: the session arrangement's groups, shared with the terminal UI.
//!
//! The active tab is simply the group holding the focused terminal, so a
//! jump from the sidebar or the inbox lands on the right tab by itself. The
//! wire carries no active group; which pane a tab returns to stays here.

use std::collections::HashMap;

use phux_client_core::layout::{ArrangementDoc, leaves};
use phux_client_runtime::Client;
use phux_client_runtime::control::SpawnRequest;
use phux_protocol::ids::{OperationId, ResourceId, SessionKey};
use rune::{Key, KeyEvent, TextEdit, Tx};

use super::{Desk, NoticeKind, Pending};
use crate::palette::pane_label;

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub id: [u8; 16],
    pub name: String,
    pub resources: Vec<ResourceId>,
    pub active: bool,
}

/// What the rename sheet is renaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renaming {
    Session(SessionKey),
    Tab([u8; 16]),
}

#[derive(Default)]
pub struct Tabs {
    pub list: Vec<Tab>,
    /// The pane each tab last had the keyboard in.
    focus: HashMap<[u8; 16], ResourceId>,
    pub renaming: Option<Renaming>,
    pub rename_field: TextEdit,
    /// The title bar's room for the strip, in points, once it is laid out:
    /// what each tab's share is cut from.
    pub room: f32,
    /// The active tab the phone's strip last scrolled to, and its label
    /// then: a longer name widens the tab past the edge again.
    revealed: Option<([u8; 16], String)>,
}

/// The group holding `shown`, which the rest of the window treats as active.
pub(super) fn focus_group(doc: &mut ArrangementDoc, shown: Option<&ResourceId>) {
    let Some(shown) = shown else { return };
    let holds = |tree: &phux_client_core::layout::LayoutNode| leaves(tree).contains(shown);
    if let Some(at) = doc
        .groups
        .iter()
        .position(|g| g.state.tree.as_ref().is_some_and(holds))
    {
        doc.active = at;
    }
}

/// The groups with a live terminal, in display order.
pub fn list(doc: &ArrangementDoc, listed: &[ResourceId]) -> Vec<Tab> {
    doc.groups
        .iter()
        .enumerate()
        .filter_map(|(at, group)| {
            let resources: Vec<ResourceId> = group
                .state
                .tree
                .iter()
                .flat_map(leaves)
                .filter(|id| listed.contains(id))
                .collect();
            (!resources.is_empty()).then(|| Tab {
                id: group.id,
                name: group.name.clone(),
                resources,
                active: at == doc.active,
            })
        })
        .collect()
}

impl Desk {
    pub(super) fn sync_tabs(&mut self, client: &Client, listed: &[ResourceId]) {
        self.tabs.list = self
            .arrangement(client)
            .map_or_else(Vec::new, |(_, doc)| list(&doc, listed));
        let shown = self.shown.as_ref().map(|s| s.resource.clone());
        if let (Some(tab), Some(shown)) = (self.tabs.list.iter().find(|t| t.active), shown) {
            self.tabs.focus.insert(tab.id, shown);
        }
    }

    /// Bring the phone strip's active tab into view when another becomes
    /// active or it is renamed, wherever that happened; between those a
    /// scroll by hand is left alone.
    pub(crate) fn reveal_active_tab(&mut self, tx: &mut Tx) {
        if self.phone.is_none() {
            return;
        }
        let active = self.tabs.list.iter().position(|t| t.active);
        let now = active.map(|at| {
            let tab = &self.tabs.list[at];
            (tab.id, self.tab_label(tab))
        });
        if now == self.tabs.revealed {
            return;
        }
        self.tabs.revealed = now;
        if let Some(at) = active {
            tx.scroll_into_view(format!("phone-tab-{at}"));
        }
    }

    /// A tab's label: its given name, else what its terminal is called
    /// (server default names are just numbers).
    pub fn tab_label(&self, tab: &Tab) -> String {
        if tab.name.parse::<u32>().is_err() && !tab.name.is_empty() {
            return tab.name.clone();
        }
        let terminal = self
            .tabs
            .focus
            .get(&tab.id)
            .filter(|id| tab.resources.contains(id))
            .or(tab.resources.first());
        terminal
            .and_then(|id| self.topology.as_ref()?.pane(id))
            .map_or_else(
                || {
                    terminal
                        .and_then(|id| self.resources.entries.get(id))
                        .and_then(|doc| doc.record.name.clone())
                        .unwrap_or_else(|| tab.name.clone())
                },
                pane_label,
            )
    }

    pub fn select_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.list.get(index) else {
            return;
        };
        let remembered = self.tabs.focus.get(&tab.id);
        let target = remembered
            .filter(|id| tab.resources.contains(id))
            .or(tab.resources.first())
            .cloned();
        let session = self.selected_session_name();
        if let (Some(target), Some(session)) = (target, session) {
            self.zoomed = None;
            self.show(target, &session);
        }
    }

    /// A new terminal in a new tab of the selected session; with nothing on
    /// screen to arrange beside, a plain terminal.
    pub fn new_tab(&mut self) {
        let Some(client) = &self.client else { return };
        if self.shown.is_none() {
            return self.new_terminal();
        }
        let request = client.spawn_terminal(SpawnRequest {
            command: None,
            cwd: self.focused_cwd(),
            env: None,
            session_id: client.selected_session(),
            initial_size: None,
        });
        self.pending_splits.insert(request, None);
        self.zoomed = None;
    }

    pub(super) fn write_tab(&mut self, client: &Client, new: &ResourceId) -> Option<OperationId> {
        self.arrangement(client).and_then(|(session, mut doc)| {
            doc.add_group(doc.default_group_name(), new.clone());
            self.submit_arrangement(client, session, &doc).ok()
        })
    }

    pub(super) fn write_arrangement(
        &mut self,
        client: &Client,
        session: SessionKey,
        doc: &ArrangementDoc,
        failure: &str,
    ) {
        if let Err(error) = self.submit_arrangement(client, session, doc) {
            eprintln!("{failure}: {error}");
            self.say(NoticeKind::Error, format!("{failure}. Try again."));
        }
    }

    /// Send `doc`, remembering the operation to hear how it went.
    pub(super) fn submit_arrangement(
        &mut self,
        client: &Client,
        session: SessionKey,
        doc: &ArrangementDoc,
    ) -> Result<OperationId, String> {
        let operation = client
            .replace_arrangement(session, doc)
            .map_err(|error| error.to_string())?;
        self.layout_ops.insert(operation);
        Ok(operation)
    }

    /// Close every terminal of a tab. With the setting on, asks first unless
    /// each one is vouched idle.
    pub fn close_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.list.get(index) else {
            return;
        };
        let terminals = tab.resources.clone();
        let (documents, terminals): (Vec<_>, Vec<_>) =
            terminals.into_iter().partition(|id| self.is_document(id));
        if !documents.is_empty() {
            self.hide_documents(&documents);
        }
        if terminals.is_empty() {
            return;
        }
        if terminals.iter().any(|id| self.asks_to_close(id)) {
            self.pending = Some(Pending::CloseTab(terminals));
        } else if let Some(client) = &self.client {
            let _ = client.close_terminals(terminals);
        }
    }

    /// Whether closing `terminal` asks first. An idle prompt never asks.
    /// With "Confirm before closing" on, anything else does. With it never
    /// chosen, a terminal an agent runs in still does, so a stray Cmd+W
    /// cannot end one; turning the setting off by hand stops that too.
    pub(super) fn asks_to_close(&self, terminal: &ResourceId) -> bool {
        if self.vouched_idle(terminal) {
            return false;
        }
        if self.settings.confirm_close {
            return true;
        }
        let chosen =
            self.file.confirm_close.is_some() || self.prefs.get::<bool>("confirm_close").is_some();
        let agents = self.topology.iter().flat_map(|t| &t.agent_sessions);
        !chosen
            && agents
                .into_iter()
                .any(|a| a.parent.as_ref() == Some(terminal))
    }

    /// Close the focused terminal: at once at a prompt shell integration
    /// vouches for, after asking otherwise. Closing is not undoable, so
    /// "unknown" asks too.
    pub fn close_terminal(&mut self) {
        // A popup is closed before anything under it.
        if self.popup.command().is_some() {
            return self.dismiss_popup();
        }
        let Some(terminal) = self.shown.as_ref().map(|s| s.resource.clone()) else {
            return;
        };
        if self.is_document(&terminal) {
            return self.hide_documents(&[terminal]);
        }
        if self.asks_to_close(&terminal) {
            self.pending = Some(Pending::Close(terminal));
        } else {
            self.close_now(&terminal);
        }
    }

    /// Whether shell integration vouches this terminal is at a prompt.
    pub(super) fn vouched_idle(&self, terminal: &ResourceId) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        match client.with_control(|control| control.command_state(terminal)) {
            Some(running) => !running,
            None => client.engine().is_some_and(|engine| {
                engine.replica_prompt(terminal)
                    == phux_client_runtime::engine::ReplicaPrompt::AtPrompt
            }),
        }
    }

    pub(super) fn close_now(&mut self, terminal: &ResourceId) {
        if self.is_document(terminal) {
            return self.hide_documents(std::slice::from_ref(terminal));
        }
        if let Some(client) = &self.client {
            let _ = client.close_terminals(vec![terminal.clone()]);
        }
    }

    pub fn close_session(&mut self) {
        let Some(id) = self.client.as_ref().and_then(|c| c.selected_session()) else {
            return;
        };
        let session = self.topology.as_ref().and_then(|t| {
            let session = t.sessions.iter().find(|s| s.id == id)?;
            Some((session.key?, session.name.clone()))
        });
        if let Some((key, name)) = session {
            self.pending = Some(Pending::CloseSession(key, name));
        }
    }

    pub fn rename_session(&mut self, key: SessionKey, name: &str) {
        self.open_rename(Renaming::Session(key), name.to_owned());
    }

    pub fn rename_selected_session(&mut self) {
        let Some(id) = self.client.as_ref().and_then(|c| c.selected_session()) else {
            return;
        };
        let session = self.topology.as_ref().and_then(|t| {
            let session = t.sessions.iter().find(|s| s.id == id)?;
            Some((session.key?, session.name.clone()))
        });
        if let Some((key, name)) = session {
            self.rename_session(key, &name);
        }
    }

    pub fn rename_tab(&mut self, index: usize) {
        if let Some(tab) = self.tabs.list.get(index) {
            let (id, label) = (tab.id, self.tab_label(tab));
            self.open_rename(Renaming::Tab(id), label);
        }
    }

    pub fn rename_active_tab(&mut self) {
        if let Some(at) = self.tabs.list.iter().position(|t| t.active) {
            self.rename_tab(at);
        }
    }

    fn open_rename(&mut self, target: Renaming, current: String) {
        self.tabs.renaming = Some(target);
        let end = current.len();
        self.tabs.rename_field = TextEdit::new(current);
        self.tabs.rename_field.set_selection(0, end);
        self.tabs.rename_field.focused = true;
    }

    pub fn cancel_rename(&mut self) {
        self.tabs.renaming = None;
        self.tabs.rename_field.focused = false;
    }

    pub fn rename_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        if event.key != Key::Escape {
            return false;
        }
        self.cancel_rename();
        tx.blur();
        true
    }

    pub fn submit_rename(&mut self, tx: &mut Tx) {
        let name = self.tabs.rename_field.text.trim().to_owned();
        let target = self.tabs.renaming.clone();
        self.cancel_rename();
        tx.blur();
        let (Some(target), Some(client)) = (target, self.client.clone()) else {
            return;
        };
        if name.is_empty() {
            return;
        }
        match target {
            Renaming::Session(key) => {
                if let Err(error) = client.rename_session(key, &name) {
                    eprintln!("Rename failed: {error}");
                    self.say(NoticeKind::Error, "Rename failed. Try again.");
                }
            }
            Renaming::Tab(id) => {
                if let Some((session, mut doc)) = self.arrangement(&client) {
                    if let Some(group) = doc.groups.iter_mut().find(|g| g.id == id) {
                        group.name = name;
                    }
                    self.write_arrangement(&client, session, &doc, "Rename failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_core::layout::{Group, LayoutState};

    fn id(n: u32) -> ResourceId {
        ResourceId::local(n)
    }

    fn doc() -> ArrangementDoc {
        ArrangementDoc {
            groups: vec![
                Group::new("1".to_owned(), LayoutState::single(id(1))),
                Group::new("2".to_owned(), LayoutState::single(id(2))),
                Group::new("gone".to_owned(), LayoutState::single(id(9))),
            ],
            active: 0,
        }
    }

    #[test]
    fn the_active_tab_is_the_one_holding_the_focus() {
        let mut d = doc();
        focus_group(&mut d, Some(&id(2)));
        assert_eq!(d.active, 1);
        focus_group(&mut d, Some(&id(7)));
        assert_eq!(d.active, 1);
    }

    #[test]
    fn a_group_with_no_live_terminal_is_not_a_tab() {
        let tabs = list(&doc(), &[id(1), id(2)]);
        assert_eq!(tabs.len(), 2);
        assert!(tabs[0].active && !tabs[1].active);
        assert_eq!(tabs[1].resources, vec![id(2)]);
    }
}
