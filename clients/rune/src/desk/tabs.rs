//! Tabs: the session arrangement's groups, shared with the terminal UI.
//!
//! The active tab is simply the group holding the focused terminal, so a
//! jump from the sidebar or the inbox lands on the right tab by itself. The
//! wire carries no active group; which pane a tab returns to stays here.

use std::collections::HashMap;

use phux_client_core::layout::{ArrangementDoc, leaves};
use phux_client_runtime::Client;
use phux_client_runtime::control::SpawnRequest;
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Key, KeyEvent, TextEdit, Tx};

use super::{Desk, Pending};
use crate::palette::pane_label;

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub id: [u8; 16],
    pub name: String,
    pub terminals: Vec<ResourceId>,
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
            let terminals: Vec<ResourceId> = group
                .state
                .tree
                .iter()
                .flat_map(leaves)
                .filter(|id| listed.contains(id))
                .collect();
            (!terminals.is_empty()).then(|| Tab {
                id: group.id,
                name: group.name.clone(),
                terminals,
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
        let shown = self.shown.as_ref().map(|s| s.terminal.clone());
        if let (Some(tab), Some(shown)) = (self.tabs.list.iter().find(|t| t.active), shown) {
            self.tabs.focus.insert(tab.id, shown);
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
            .filter(|id| tab.terminals.contains(id))
            .or(tab.terminals.first());
        terminal
            .and_then(|id| self.topology.as_ref()?.pane(id))
            .map_or_else(|| tab.name.clone(), pane_label)
    }

    pub fn select_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.list.get(index) else {
            return;
        };
        let remembered = self.tabs.focus.get(&tab.id);
        let target = remembered
            .filter(|id| tab.terminals.contains(id))
            .or(tab.terminals.first())
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
            cwd: None,
            env: None,
            session_id: client.selected_session(),
            initial_size: None,
        });
        self.pending_splits.insert(request, None);
        self.zoomed = None;
    }

    pub(super) fn write_tab(&mut self, client: &Client, new: &ResourceId) {
        if let Some((session, mut doc)) = self.arrangement(client) {
            doc.add_group(doc.default_group_name(), new.clone());
            self.write_arrangement(client, session, &doc, "Tab not saved");
        }
        self.want(new.clone());
    }

    pub(super) fn write_arrangement(
        &mut self,
        client: &Client,
        session: SessionKey,
        doc: &ArrangementDoc,
        failure: &str,
    ) {
        match client.replace_arrangement(session, doc) {
            Ok(operation) => {
                self.layout_ops.insert(operation);
            }
            Err(error) => self.notice = Some(format!("{failure}: {error}")),
        }
    }

    /// Close every terminal of a tab, after asking: this ends what runs there.
    pub fn close_tab(&mut self, index: usize) {
        if let Some(tab) = self.tabs.list.get(index) {
            self.pending = Some(Pending::CloseTab(tab.terminals.clone()));
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
                    self.notice = Some(format!("Rename failed: {error}"));
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
        assert_eq!(tabs[1].terminals, vec![id(2)]);
    }
}
