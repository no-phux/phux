//! Back-to-front surface order, shared by composition and input ownership.
//!
//! Settings and the theme palette may deliberately nest. Playground scenes,
//! in contrast, share one active slot rather than competing open flags.

use super::Desk;
use super::input::Modal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playground {
    Galaxy,
    Reef,
    Pitch,
    Kitchen,
    PitLane,
    Hive,
}

impl Playground {
    const fn modal(self) -> Modal {
        match self {
            Self::Galaxy => Modal::Galaxy,
            Self::Reef => Modal::Reef,
            Self::Pitch => Modal::Pitch,
            Self::Kitchen => Modal::Kitchen,
            Self::PitLane => Modal::PitLane,
            Self::Hive => Modal::Hive,
        }
    }
}

/// Who opened a kit menu (`rune_kit::Menu`, `context_menu`): its open
/// state lives in `Desk`, and its keys reach the menu, not the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOwner {
    /// A terminal's context menu.
    Pane,
    /// A tab's context menu.
    Tab,
    /// A sidebar row's context menu.
    Sidebar,
}

/// Which kit select (`rune_kit::Select`) is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectOwner {
    /// Settings' terminal font.
    Font,
}

/// Only floating surfaces belong here. In-pane find, confirmation and the
/// onboarding stage keep their own layout and are considered below these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    PhoneNavigation,
    Inbox,
    Playground(Playground),
    Settings,
    Machines,
    News,
    Changes,
    Popup,
    Rename,
    Palette,
    ContextMenu,
    /// A kit menu, drawn in the kit layer.
    Menu(MenuOwner),
    /// A kit select's popup list, drawn in the kit layer.
    Select(SelectOwner),
    /// The toast region: painted, but never the keyboard's owner.
    Toasts,
}

impl Surface {
    /// The input owner this surface is, if any: toasts never take the
    /// keyboard from the terminal.
    pub(super) const fn modal(self) -> Option<Modal> {
        Some(match self {
            Self::PhoneNavigation => Modal::PhoneNavigation,
            Self::Inbox => Modal::Inbox,
            Self::Playground(scene) => scene.modal(),
            Self::Settings => Modal::Settings,
            Self::Machines => Modal::Machines,
            Self::News => Modal::News,
            Self::Changes => Modal::Changes,
            Self::Popup => Modal::Popup,
            Self::Rename => Modal::Rename,
            Self::Palette => Modal::Palette,
            Self::ContextMenu | Self::Menu(_) | Self::Select(_) => Modal::ContextMenu,
            Self::Toasts => return None,
        })
    }
}

impl Desk {
    pub(super) fn tick_playground(&mut self, tx: &mut rune::Tx) -> bool {
        match self.playground {
            None => false,
            Some(Playground::Galaxy) => self.tick_galaxy(tx),
            Some(Playground::Reef) => self.tick_reef(tx),
            Some(Playground::Pitch) => self.tick_pitch(tx),
            Some(Playground::Kitchen) => self.tick_kitchen(tx),
            Some(Playground::PitLane) => self.tick_pitlane(tx),
            Some(Playground::Hive) => self.tick_hive(tx),
        }
    }

    /// Active surfaces in their paint order. The last one owns input; phone
    /// Settings is a page in the stage, below its floating navigation.
    pub fn surfaces(&self) -> impl DoubleEndedIterator<Item = Surface> {
        [
            (self.phone.is_some() && self.settings_open).then_some(Surface::Settings),
            self.phone_page().map(|_| Surface::PhoneNavigation),
            self.inbox.open.then_some(Surface::Inbox),
            self.playground
                .filter(|_| self.phone.is_none() || !self.settings_open)
                .map(Surface::Playground),
            (self.phone.is_none() && self.settings_open).then_some(Surface::Settings),
            self.machines.open.then_some(Surface::Machines),
            self.news.open.then_some(Surface::News),
            self.connection.changes.open.then_some(Surface::Changes),
            self.connection.popup.command().map(|_| Surface::Popup),
            self.connection
                .tabs
                .renaming
                .as_ref()
                .map(|_| Surface::Rename),
            self.palette_open.then_some(Surface::Palette),
            self.pointer.menu.map(|_| Surface::ContextMenu),
        ]
        .into_iter()
        .flatten()
    }

    pub(super) fn foreground(&self) -> Option<Modal> {
        let surface = self.surfaces().rev().find(|s| s.modal().is_some())?;
        // A popup asks inside its own box, above its terminal, not behind it.
        if surface == Surface::Popup && self.pending.is_some() {
            return Some(Modal::Confirm);
        }
        surface.modal()
    }

    pub(super) fn close_playground(&mut self, scene: Playground) {
        if self.playground == Some(scene) {
            self.playground = None;
        }
    }
}

#[cfg(test)]
mod tests;
