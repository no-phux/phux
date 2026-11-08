//! Shared input ownership policy, including phone-only surfaces. Rune's
//! pointer modal and the app's keyboard/IME/paste gate must agree.

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
    Machines,
    Settings,
    Rename,
    Palette,
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
            Self::Settings => matches!(action, Shortcut::Find) || action.works_over_sheets(),
            Self::Inbox | Self::Galaxy | Self::Reef | Self::Pitch | Self::Kitchen => {
                action.works_over_sheets()
            }
            Self::ContextMenu
            | Self::Machines
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
