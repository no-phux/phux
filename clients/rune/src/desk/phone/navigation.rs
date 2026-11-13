//! Phone destinations are navigation, not aliases for command search.

use rune::{Key, KeyEvent, Tx};

use super::{Desk, Soft};
use crate::keys::Shortcut;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Sessions,
    Actions,
}

impl Desk {
    pub(in crate::desk) fn switch_sessions(&mut self) {
        if self.phone.is_none() {
            return self.open_palette(crate::palette::Mode::Sessions);
        }
        self.close_palette();
        self.close_inbox();
        self.close_settings();
        self.open_phone_sessions();
    }

    pub fn phone_page(&self) -> Option<Page> {
        self.phone.as_ref()?.page
    }

    pub fn open_phone_sessions(&mut self) {
        self.open_phone_navigation(Page::Sessions);
    }

    pub fn open_phone_actions(&mut self) {
        self.open_phone_navigation(Page::Actions);
    }

    fn open_phone_navigation(&mut self, page: Page) {
        if self.modal().is_some() {
            return;
        }
        if let Some(phone) = &mut self.phone {
            phone.reset_navigation();
            phone.release();
            phone.page = Some(page);
            phone.query.clear();
            phone.query.focused = false;
        }
    }

    pub fn close_phone_navigation(&mut self) {
        if let Some(phone) = &mut self.phone {
            phone.page = None;
            phone.query.clear();
            phone.query.focused = false;
            phone.query.cancel_composition();
            phone.release();
        }
    }

    /// Hiding search preserves the filter and the terminal's keyboard choice.
    pub fn blur_phone_search(&mut self, tx: &mut Tx) {
        if let Some(phone) = &mut self.phone {
            phone.query.focused = false;
            phone.query.cancel_composition();
            phone.release();
        }
        tx.blur();
        tx.instant();
    }

    pub fn phone_navigation_key(&mut self, event: &KeyEvent) -> bool {
        if event.key == Key::Escape {
            self.close_phone_navigation();
            return true;
        }
        false
    }

    pub(super) fn phone_navigation_soft(&mut self, soft: Soft, tx: &mut Tx) {
        let Some(phone) = &mut self.phone else { return };
        let event = KeyEvent {
            key: phone.for_field(soft),
            mods: rune::Mods::default(),
        };
        if self.phone_navigation_key(&event) {
            return;
        }
        if let Some(phone) = &mut self.phone
            && phone.query.focused
        {
            let outcome = phone.query.key(&event, None);
            if outcome.submit || outcome.blur {
                self.blur_phone_search(tx);
            }
        }
    }

    pub fn phone_action(&mut self, action: Shortcut, tx: &mut Tx) {
        self.close_phone_navigation();
        tx.blur();
        self.shortcut(action, tx);
    }

    pub fn phone_session(&mut self, name: &str) {
        self.close_phone_navigation();
        self.show_session(name);
    }

    pub fn phone_create_session(&mut self) {
        let name = self
            .phone
            .as_ref()
            .map(|phone| phone.query.text.trim().to_owned());
        self.close_phone_navigation();
        match name.filter(|name| !name.is_empty()) {
            Some(name) => self.create_session(name),
            None => self.new_session(),
        }
    }
}
