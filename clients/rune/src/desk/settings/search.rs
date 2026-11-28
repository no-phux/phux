//! The Settings search field: editing from every input path, clearing, and
//! where its results start.

use rune::{Key, KeyEvent, Tx};

use super::super::Desk;

impl Desk {
    pub fn clear_settings_search(&mut self, tx: &mut Tx) {
        self.settings_ui.query.clear();
        self.focus_settings_search();
        self.settings_search_edited(tx);
    }

    /// A changed search starts at its first result, not the old results'
    /// offset. Shared by hardware, drawn and UIKit editing and by clearing;
    /// a caret move changes no text and keeps the place.
    pub fn settings_search_edited(&mut self, tx: &mut Tx) {
        tx.scroll_to_instant(self.settings_scroller(), 0.0, 0.0);
        tx.instant();
    }

    pub fn focus_settings_search(&mut self) {
        self.settings_ui.query.focused = true;
    }

    pub fn blur_settings_search(&mut self, tx: &mut Tx) {
        self.settings_ui.query.focused = false;
        tx.blur();
    }

    pub fn settings_search_focused(&self) -> bool {
        self.settings_open && self.settings_ui.query.focused
    }

    /// Native fields keep editing, selection, clipboard and Tab with Rune.
    pub fn settings_field_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        match event.key {
            Key::Escape if !self.settings_ui.query.text.is_empty() => {
                self.clear_settings_search(tx);
            }
            Key::Escape => self.settings_dismiss(),
            Key::Enter => self.blur_settings_search(tx),
            _ => return false,
        }
        if !self.settings_search_focused() {
            tx.blur();
        }
        true
    }

    fn settings_dismiss(&mut self) {
        if self.phone.is_some() && self.settings_ui.category.is_some() {
            self.settings_back();
        } else {
            self.close_settings();
        }
    }

    pub fn settings_escape(&mut self, tx: &mut Tx) {
        if self.settings_ui.query.text.is_empty() {
            self.settings_dismiss();
        } else {
            self.clear_settings_search(tx);
        }
    }

    /// Drawn keys edit this field now, never queued into a later frame where
    /// the sheet may have closed and the terminal could have the keyboard.
    pub fn settings_soft(&mut self, event: &KeyEvent, tx: &mut Tx) {
        if !self.settings_search_focused() || self.settings_field_key(event, tx) {
            return;
        }
        if event.key == Key::Tab {
            self.blur_settings_search(tx);
            return;
        }
        let before = self.settings_ui.query.text.clone();
        self.settings_ui.query.key(event, None);
        if self.settings_ui.query.text != before {
            self.settings_search_edited(tx);
        }
    }

    /// Unhandled arrows never mutate preferences. Escape backs out of the
    /// phone category before closing the sheet.
    pub(in crate::desk) fn settings_key(&mut self, event: &KeyEvent, tx: &mut Tx) {
        if self.settings_search_focused() && tx.focused().is_none() {
            self.settings_soft(event, tx);
        } else if event.key == Key::Escape {
            self.settings_dismiss();
        } else if !self.settings_ui.query.focused && starts_search(event) {
            // Typing anywhere but a field searches, as System Settings does:
            // a focused button (the gear that opened the sheet) takes no text.
            self.focus_settings_search();
            self.settings_soft(event, tx);
        }
    }
}

/// A printable key with no chord modifier: the start of a search.
fn starts_search(event: &KeyEvent) -> bool {
    let chord = event.mods.cmd || event.mods.ctrl || event.mods.alt || event.mods.logo;
    matches!(&event.key, Key::Text(t) if !chord && !t.chars().any(char::is_control))
}
