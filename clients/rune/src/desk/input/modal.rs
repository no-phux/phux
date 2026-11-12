//! Shared input ownership policy, including phone-only surfaces. Rune's
//! pointer modal and the app's keyboard/IME/paste gate must agree.

use crate::desk::Desk;
use crate::keys::Shortcut;

/// What has the keyboard instead of the terminal, in the order they claim it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    ContextMenu,
    Inbox,
    Galaxy,
    Reef,
    Pitch,
    Kitchen,
    PitLane,
    Hive,
    Machines,
    News,
    Changes,
    Settings,
    Rename,
    Palette,
    Onboarding,
    FindField,
    Confirm,
    PhonePicker,
}

impl Modal {
    /// Whether a command, by chord or menu pick, may run while this has the
    /// keyboard. The find field leaves the shown terminal in view; a sheet
    /// hides what a command would touch.
    pub(in crate::desk) const fn allows(self, action: Shortcut) -> bool {
        match self {
            // Rune owns editing the field. Extended clipboard chords now
            // reach the app too; they must never address the terminal behind it.
            Self::FindField => !matches!(
                action,
                Shortcut::Copy | Shortcut::Paste | Shortcut::SelectAll
            ),
            Self::Palette => matches!(action, Shortcut::Palette),
            Self::Onboarding => matches!(action, Shortcut::Palette) || action.works_over_sheets(),
            Self::Settings => matches!(action, Shortcut::Find) || action.works_over_sheets(),
            Self::Inbox
            | Self::Galaxy
            | Self::Reef
            | Self::Pitch
            | Self::Kitchen
            | Self::PitLane
            | Self::Hive => action.works_over_sheets(),
            Self::ContextMenu
            | Self::Machines
            | Self::News
            | Self::Changes
            | Self::Rename
            | Self::Confirm
            | Self::PhonePicker => false,
        }
    }

    /// Whether this covers the panes as a Rune modal, which drops any held
    /// press without its release. Find and confirmation live inside a pane.
    pub(in crate::desk) const fn takes_pointer(self) -> bool {
        !matches!(self, Self::FindField | Self::Confirm)
    }
}

impl Desk {
    /// Escape and Enter over a playground view.
    pub(in crate::desk) fn navigation_sheet_key(
        &mut self,
        modal: Modal,
        event: &rune::KeyEvent,
    ) -> bool {
        match (modal, &event.key) {
            (Modal::Galaxy, rune::Key::Escape) => self.close_galaxy(),
            (Modal::Galaxy, rune::Key::Enter) => self.open_picked(),
            (Modal::Reef, rune::Key::Escape) => self.reef_escape(),
            (Modal::Reef, rune::Key::Enter) => self.open_fish(),
            (Modal::Pitch, rune::Key::Escape) => self.pitch_escape(),
            (Modal::Pitch, rune::Key::Enter) => self.open_player(),
            (Modal::Kitchen, rune::Key::Escape) => self.kitchen_escape(),
            (Modal::Kitchen, rune::Key::Enter) => self.open_chef(),
            (Modal::PitLane, rune::Key::Escape) => self.pitlane_escape(),
            (Modal::PitLane, rune::Key::Enter) => self.open_car(),
            (Modal::Hive, rune::Key::Escape) => self.hive_escape(),
            (Modal::Hive, rune::Key::Enter) => self.open_bee(),
            _ => return false,
        }
        true
    }
}
