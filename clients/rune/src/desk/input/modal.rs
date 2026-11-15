//! Shared input ownership policy, including phone-only surfaces. Rune's
//! pointer modal and the app's keyboard/IME/paste gate must agree.

use crate::desk::Desk;
use crate::keys::Shortcut;
use crate::{hive::Hive, kitchen::Kitchen, pitch::Pitch, pitlane::PitLane, reef::Reef};

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
    /// A popup's command: typing is its own, and few commands reach past it.
    Popup,
    PhonePicker,
    PhoneNavigation,
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
            Self::Popup => matches!(
                action,
                Shortcut::Copy | Shortcut::Paste | Shortcut::CloseTerminal
            ),
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
            | Self::PhoneNavigation
            | Self::PhonePicker => false,
        }
    }

    /// Whether this covers the panes as a Rune modal, which drops any held
    /// press without its release. Find and confirmation live inside a pane,
    /// and a popup is one.
    pub(in crate::desk) const fn takes_pointer(self) -> bool {
        !matches!(self, Self::FindField | Self::Confirm | Self::Popup)
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
            (Modal::Reef, rune::Key::Escape) => self.scene_escape::<Reef>(),
            (Modal::Reef, rune::Key::Enter) => self.open_agent::<Reef>(),
            (Modal::Pitch, rune::Key::Escape) => self.scene_escape::<Pitch>(),
            (Modal::Pitch, rune::Key::Enter) => self.open_agent::<Pitch>(),
            (Modal::Kitchen, rune::Key::Escape) => self.scene_escape::<Kitchen>(),
            (Modal::Kitchen, rune::Key::Enter) => self.open_agent::<Kitchen>(),
            (Modal::PitLane, rune::Key::Escape) => self.scene_escape::<PitLane>(),
            (Modal::PitLane, rune::Key::Enter) => self.open_agent::<PitLane>(),
            (Modal::Hive, rune::Key::Escape) => self.scene_escape::<Hive>(),
            (Modal::Hive, rune::Key::Enter) => self.open_agent::<Hive>(),
            _ => return false,
        }
        true
    }
}
