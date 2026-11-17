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

/// A playground view's row: what has the keyboard while it is open, and
/// the Desk's ways in, out and through it.
#[derive(Clone, Copy)]
pub struct Scene {
    pub playground: Playground,
    pub modal: Modal,
    pub toggle: fn(&mut Desk),
    pub close: fn(&mut Desk),
    /// Escape: lets go of a pick first, then closes.
    pub escape: fn(&mut Desk),
    /// Return: opens the pick.
    pub enter: fn(&mut Desk),
    tick: fn(&mut Desk, &mut rune::Tx) -> bool,
}

impl Playground {
    /// Every playground view, in the enum's order. A new view is one more row.
    pub const ALL: [Scene; 6] = [
        Scene {
            playground: Self::Galaxy,
            modal: Modal::Galaxy,
            toggle: Desk::toggle_galaxy,
            close: Desk::close_galaxy,
            escape: Desk::close_galaxy,
            enter: Desk::open_picked,
            tick: Desk::tick_galaxy,
        },
        Scene {
            playground: Self::Reef,
            modal: Modal::Reef,
            toggle: Desk::toggle_reef,
            close: Desk::close_reef,
            escape: Desk::reef_escape,
            enter: Desk::open_fish,
            tick: Desk::tick_scene::<crate::reef::Reef>,
        },
        Scene {
            playground: Self::Pitch,
            modal: Modal::Pitch,
            toggle: Desk::toggle_pitch,
            close: Desk::close_pitch,
            escape: Desk::pitch_escape,
            enter: Desk::open_player,
            tick: Desk::tick_scene::<crate::pitch::Pitch>,
        },
        Scene {
            playground: Self::Kitchen,
            modal: Modal::Kitchen,
            toggle: Desk::toggle_kitchen,
            close: Desk::close_kitchen,
            escape: Desk::kitchen_escape,
            enter: Desk::open_chef,
            tick: Desk::tick_scene::<crate::kitchen::Kitchen>,
        },
        Scene {
            playground: Self::PitLane,
            modal: Modal::PitLane,
            toggle: Desk::toggle_pitlane,
            close: Desk::close_pitlane,
            escape: Desk::pitlane_escape,
            enter: Desk::open_car,
            tick: Desk::tick_scene::<crate::pitlane::PitLane>,
        },
        Scene {
            playground: Self::Hive,
            modal: Modal::Hive,
            toggle: Desk::toggle_hive,
            close: Desk::close_hive,
            escape: Desk::hive_escape,
            enter: Desk::open_bee,
            tick: Desk::tick_scene::<crate::hive::Hive>,
        },
    ];

    /// This view's row.
    pub fn scene(self) -> Scene {
        Self::ALL[self as usize]
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
    /// A kit menu (a terminal's or a tab's context menu among them), drawn in the kit layer.
    Menu(MenuOwner),
    /// A kit select's popup list, drawn in the kit layer.
    Select(SelectOwner),
    /// The toast region: painted, but never the keyboard's owner.
    Toasts,
}

impl Surface {
    /// The input owner this surface is, if any: toasts never take the
    /// keyboard from the terminal.
    pub(super) fn modal(self) -> Option<Modal> {
        Some(match self {
            Self::PhoneNavigation => Modal::PhoneNavigation,
            Self::Inbox => Modal::Inbox,
            Self::Playground(scene) => scene.scene().modal,
            Self::Settings => Modal::Settings,
            Self::Machines => Modal::Machines,
            Self::News => Modal::News,
            Self::Changes => Modal::Changes,
            Self::Popup => Modal::Popup,
            Self::Rename => Modal::Rename,
            Self::Palette => Modal::Palette,
            Self::Menu(_) | Self::Select(_) => Modal::ContextMenu,
            Self::Toasts => return None,
        })
    }
}

impl Desk {
    pub(super) fn tick_playground(&mut self, tx: &mut rune::Tx) -> bool {
        self.playground
            .is_some_and(|scene| (scene.scene().tick)(self, tx))
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
        ]
        .into_iter()
        .flatten()
        .chain(self.kit_surfaces())
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
