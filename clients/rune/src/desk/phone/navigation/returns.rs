//! A page's children come back to it: Inbox, Settings, What's new and the
//! desktop-hosted Machines sheet opened from a navigation page return to
//! that page on closing, however they close, rather than past it to the
//! terminal. Connections on iOS is UIKit's sheet over the page instead.

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::{Desk, Page};
use crate::keys::Shortcut;

/// A destination a navigation page opens in its own place. Closing it goes
/// back to that page rather than past it to the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Child {
    Inbox,
    Settings,
    /// The desktop-hosted phone's Machines sheet. Native Connections is
    /// UIKit's sheet over the page, which never closes for it.
    Machines,
    News,
}

/// The page a child was opened from, and the pane on screen then: a child
/// that leaves for another pane does not come back.
#[derive(Debug, Clone, PartialEq)]
pub struct Return {
    page: Page,
    child: Child,
    pane: Option<ResourceId>,
}

impl Desk {
    /// A page's row runs `action`. A destination it opens in the page's
    /// place (Inbox, Settings, Machines) comes back to the page on closing;
    /// Connections on iOS comes up over the page instead.
    pub fn phone_action(&mut self, action: Shortcut, tx: &mut Tx) {
        if action == Shortcut::Machines {
            return self.phone_connections(tx);
        }
        let from = self.phone_page();
        self.close_phone_navigation();
        tx.blur();
        self.shortcut(action, tx);
        let child = match action {
            Shortcut::Inbox => Some(Child::Inbox),
            Shortcut::Settings => Some(Child::Settings),
            _ => None,
        };
        if let Some((page, child)) = from.zip(child) {
            self.remember_return(page, child);
        }
    }

    /// Connections from a page: UIKit's sheet over it on iOS, which leaves
    /// the page where it is; the desktop-hosted phone's Machines sheet in
    /// its place, coming back to it on closing.
    pub fn phone_connections(&mut self, tx: &mut Tx) {
        tx.blur();
        if self.native.is_some() {
            if let Some(phone) = &mut self.phone {
                phone.query.focused = false;
            }
            return self.toggle_machines();
        }
        let from = self.phone_page();
        self.close_phone_navigation();
        self.toggle_machines();
        if let Some(page) = from {
            self.remember_return(page, Child::Machines);
        }
    }

    /// What's new from Terminal actions, coming back to them on closing.
    pub fn phone_news(&mut self, tx: &mut Tx) {
        let from = self.phone_page();
        self.close_phone_navigation();
        tx.blur();
        if !self.news.open {
            self.toggle_news();
        }
        if let Some(page) = from {
            self.remember_return(page, Child::News);
        }
    }

    /// Note that `child`, now open in `page`'s place, goes back to it.
    fn remember_return(&mut self, page: Page, child: Child) {
        if !self.child_open(child) {
            return;
        }
        let pane = self.connection.shown.as_ref().map(|s| s.resource.clone());
        if let Some(phone) = &mut self.phone {
            phone.return_to = Some(Return { page, child, pane });
        }
    }

    fn child_open(&self, child: Child) -> bool {
        match child {
            Child::Inbox => self.inbox.open,
            Child::Settings => self.settings_open,
            Child::Machines => self.machines.open,
            Child::News => self.news.open,
        }
    }

    /// Once a child a page opened has closed, however it closed (its
    /// control, Escape, a swipe, a tap outside): back to that page. Not if
    /// something else took the screen meanwhile, or another pane did.
    pub(in crate::desk) fn phone_return(&mut self, tx: &mut Tx) -> bool {
        let Some(back) = self.phone.as_ref().and_then(|p| p.return_to.clone()) else {
            return false;
        };
        if self.child_open(back.child) {
            return false;
        }
        if let Some(phone) = &mut self.phone {
            phone.return_to = None;
        }
        let shown = self.connection.shown.as_ref().map(|s| &s.resource);
        if self.modal().is_some() || shown != back.pane.as_ref() {
            return false;
        }
        // The child's closing was the input that caused this: the page
        // comes back on its own motion, not at once.
        tx.animated();
        match back.page {
            Page::Sessions => self.open_phone_sessions(),
            Page::Actions => self.open_phone_actions(),
            Page::Switcher => self.open_phone_switcher(),
        }
        true
    }
}
