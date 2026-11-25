//! Keyboard picks: a pane by its number (`display-panes`), the sidebar's
//! rows (`focus-sidebar`), typing into every terminal of the tab
//! (`sync-panes`), and back to the last pane or tab (`last-pane`,
//! `last-tab`, a click on the tab already shown).

use phux_client_core::layout::leaves;
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::{Desk, NoticeKind};

impl Desk {
    /// Number the tab's panes, the first nine, until the next key.
    pub(super) fn display_panes(&mut self) {
        let mut panes = self.navigation_tree().map(leaves).unwrap_or_default();
        panes.truncate(9);
        self.pane_numbers = panes;
    }

    /// A key while the numbers show: a digit focuses its pane, and any key
    /// puts the numbers away. True when the numbers took it.
    pub(super) fn pane_number_key(&mut self, event: &rune::KeyEvent) -> bool {
        if self.pane_numbers.is_empty() {
            return false;
        }
        let panes = std::mem::take(&mut self.pane_numbers);
        let digit = match &event.key {
            rune::Key::Text(text) if !event.mods.cmd && !event.mods.ctrl => {
                text.parse::<usize>().ok()
            }
            _ => None,
        };
        if let Some(pane) = digit.and_then(|n| panes.get(n.checked_sub(1)?)) {
            self.focus_pane(&pane.clone());
        }
        true
    }

    /// Hand the keyboard to the row containing the shown resource, revealing
    /// the sidebar and its session first when either is folded.
    pub(super) fn focus_sidebar(&mut self, tx: &mut Tx) {
        if !self.sidebar_shown() {
            self.toggle_sidebar();
        }
        if let Some(shown) = &self.connection.shown
            && let Some((row, session)) = crate::ui::sidebar_focus_target(self, &shown.resource)
        {
            if let Some(key) = session {
                self.sidebar_collapsed.remove(&key);
            }
            self.focus_chrome(format!("pane-{row}"), tx);
        }
    }

    /// Remember what the keyboard leaves for `new`: the terminal, and its
    /// tab when `new` is in another (or a tab not listed yet).
    pub(super) fn leaving(&mut self, old: &ResourceId, new: &ResourceId) {
        if old == new {
            return;
        }
        let tabs = &mut self.connection.tabs;
        let tab_of = |id: &ResourceId| {
            let tab = tabs.list.iter().find(|tab| tab.resources.contains(id));
            tab.map(|tab| tab.id)
        };
        let (from, to) = (tab_of(old), tab_of(new));
        if from.is_some() && from != to {
            tabs.left_tab = from;
        }
        tabs.last_pane = Some(old.clone());
    }

    /// `last-pane`: back to the terminal the keyboard left last, in
    /// whichever tab holds it.
    pub(super) fn last_pane(&mut self) {
        let Some(pane) = self.connection.tabs.last_pane.clone() else {
            return;
        };
        let topology = self.connection.topology.as_ref();
        let Some(session) = topology
            .and_then(|t| t.pane(&pane))
            .map(|p| p.session_name.clone())
        else {
            return;
        };
        self.show(pane, &session);
    }

    /// `last-tab`: back to the tab the keyboard left last, at its pane.
    pub(super) fn last_tab(&mut self) {
        let left = self.connection.tabs.left_tab;
        let list = &self.connection.tabs.list;
        if let Some(index) = list
            .iter()
            .position(|tab| Some(tab.id) == left && !tab.active)
        {
            self.select_tab(index);
        }
    }

    /// A click on a tab: selects it, or back to the last tab when it is
    /// the one shown, as tmux's `select-window -T` does.
    pub fn click_tab(&mut self, index: usize) {
        match self.connection.tabs.list.get(index) {
            Some(tab) if tab.active => self.last_tab(),
            _ => self.select_tab(index),
        }
    }

    /// The active tab: its id and terminals.
    fn active_tab_of(&self) -> Option<&super::tabs::Tab> {
        self.connection.tabs.list.iter().find(|tab| tab.active)
    }

    /// Turn typing into every terminal of the tab on, or off.
    pub(super) fn toggle_sync(&mut self) {
        let tab = self.active_tab_of().map(|tab| tab.id);
        self.sync_tab = if self.sync_tab == tab { None } else { tab };
        let said = if self.sync_tab.is_some() {
            "Typing goes to every pane in this tab."
        } else {
            "Typing goes to the focused pane only."
        };
        self.say(NoticeKind::Info, said);
    }

    /// Whether `terminal` shares what is typed: its tab is the active one
    /// and has sync on.
    pub fn synced(&self, terminal: &ResourceId) -> bool {
        self.active_tab_of()
            .is_some_and(|tab| Some(tab.id) == self.sync_tab && tab.resources.contains(terminal))
    }

    /// The other terminals that get what `focused` gets.
    pub(super) fn sync_others(&self, focused: &ResourceId) -> Vec<ResourceId> {
        if !self.synced(focused) {
            return Vec::new();
        }
        let tab = self
            .active_tab_of()
            .map_or(&[][..], |tab| &tab.resources[..]);
        tab.iter()
            .filter(|id| *id != focused && !self.is_document(id))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use phux_client_core::layout::{LayoutNode, SplitDir};
    use phux_protocol::ids::ResourceId;

    fn key(key: rune::Key) -> rune::KeyEvent {
        rune::KeyEvent {
            key,
            mods: rune::Mods::default(),
        }
    }

    #[test]
    fn numbers_follow_the_layout_and_any_key_puts_them_away() {
        let mut desk = crate::test_support::window();
        let split = |a: u32, b: u32| LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(ResourceId::local(a))),
            right: Box::new(LayoutNode::Leaf(ResourceId::local(b))),
        };
        desk.connection.layout = Some(split(4, 2));
        desk.display_panes();
        assert_eq!(desk.pane_numbers, [4, 2].map(ResourceId::local));
        assert!(desk.pane_number_key(&key(rune::Key::Text("2".into()))));
        assert!(desk.pane_numbers.is_empty());
        assert!(!desk.pane_number_key(&key(rune::Key::Text("x".into()))));
        desk.display_panes();
        assert!(desk.pane_number_key(&key(rune::Key::Escape)));
        assert!(desk.pane_numbers.is_empty());
    }

    #[test]
    fn leaving_remembers_the_pane_and_the_tab_to_go_back_to() {
        let mut desk = crate::test_support::window();
        let id = ResourceId::local;
        let tab = |n: u8, ids: &[u32], active| crate::desk::Tab {
            id: [n; 16],
            name: "t".into(),
            resources: ids.iter().map(|&i| id(i)).collect(),
            active,
            locked: false,
        };
        desk.connection.tabs.list = vec![tab(1, &[1, 2], true), tab(2, &[3], false)];
        desk.leaving(&id(1), &id(2));
        assert_eq!(desk.connection.tabs.last_pane, Some(id(1)));
        assert_eq!(desk.connection.tabs.left_tab, None, "same tab");
        desk.leaving(&id(2), &id(3));
        assert_eq!(desk.connection.tabs.last_pane, Some(id(2)));
        assert_eq!(desk.connection.tabs.left_tab, Some([1; 16]));
        // A new tab's terminal is in no listed tab yet.
        desk.leaving(&id(3), &id(9));
        assert_eq!(desk.connection.tabs.left_tab, Some([2; 16]));
    }

    #[test]
    fn sync_covers_the_active_tab_until_toggled_off() {
        let mut desk = crate::test_support::window();
        let ids = [1, 2, 3].map(ResourceId::local);
        desk.connection.tabs.list.push(crate::desk::Tab {
            id: [7; 16],
            name: "t".into(),
            resources: ids.to_vec(),
            active: true,
            locked: false,
        });
        assert!(desk.sync_others(&ids[0]).is_empty());
        desk.toggle_sync();
        assert!(desk.synced(&ids[1]) && !desk.synced(&ResourceId::local(9)));
        assert_eq!(desk.sync_others(&ids[0]), ids[1..]);
        desk.toggle_sync();
        assert!(desk.sync_others(&ids[0]).is_empty());
    }
}
