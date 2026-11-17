//! State the rune-kit components keep between view runs. A kit component
//! is a builder over app state (`Menu::open`, `Select::open`,
//! `TextInput::new(&edit)`), so what it shows lives here and its handlers
//! write back through these methods.
//!
//! - The open kit menu: whose it is, where it opened and which submenu is
//!   out. The terminal and tab context menus keep `Pointer::menu` as their
//!   anchor, so the menu and the anchor never disagree.
//! - The open kit select and its keyboard highlight.
//! - [`KitField`]: every app-owned text field a kit `TextInput` or
//!   `TextArea` edits, by name.

use rune::{PointerPress, TextEdit};

use super::Desk;
use super::surfaces::{MenuOwner, SelectOwner, Surface};
use crate::grid;

/// A kit menu that is open.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpenMenu {
    pub owner: MenuOwner,
    /// The window point a context menu opened at; `None` for a menu that
    /// hangs from its trigger.
    pub at: Option<PointerPress>,
    /// The entry whose submenu is out, by index.
    pub submenu: Option<usize>,
}

/// The kit select that is open, and the option the keyboard is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSelect {
    pub owner: SelectOwner,
    pub highlighted: Option<String>,
}

#[derive(Debug, Default)]
pub struct KitState {
    /// A menu that is not a context menu (those are `Pointer::menu`).
    menu: Option<OpenMenu>,
    select: Option<OpenSelect>,
    /// What `field` shows for a field this window lacks (the phone's
    /// search on a desktop window): always empty.
    empty: TextEdit,
    /// Where `field_mut` sends edits to such a field; emptied each time.
    spare: TextEdit,
}

/// The app-owned text fields kit inputs edit. `Desk::field` and
/// `Desk::field_mut` reach each, so a view writes
/// `TextInput::new(desk.field(f)).on_edit(move |d: &mut Desk| d.field_mut(f))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KitField {
    Palette,
    Find,
    SettingsSearch,
    Rename,
    InboxReply,
    Machine,
    /// The phone's session search; the desktop has none.
    PhoneSessions,
}

impl KitField {
    pub const ALL: [Self; 7] = [
        Self::Palette,
        Self::Find,
        Self::SettingsSearch,
        Self::Rename,
        Self::InboxReply,
        Self::Machine,
        Self::PhoneSessions,
    ];

    /// The element key the field is drawn with, which the native keyboard
    /// bridge resolves (`native_input::target`).
    pub const fn key(self) -> &'static str {
        match self {
            Self::Palette => "palette-query",
            Self::Find => "find-query",
            Self::SettingsSearch => "settings-query",
            Self::Rename => "rename-field",
            Self::InboxReply => "inbox-reply",
            Self::Machine => "machine-field",
            Self::PhoneSessions => "phone-session-query",
        }
    }
}

impl KitState {
    pub(super) fn close_menu(&mut self) {
        self.menu = None;
    }
}

impl Desk {
    /// The open kit menu: a context menu at its point, or a trigger's.
    pub fn open_menu(&self) -> Option<OpenMenu> {
        let context = self.pointer.menu.map(|at| OpenMenu {
            owner: if self.pointer.tab_menu {
                MenuOwner::Tab
            } else {
                MenuOwner::Pane
            },
            at: Some(at),
            submenu: self.kit.menu.and_then(|m| m.submenu),
        });
        context.or(self.kit.menu)
    }

    /// Whether `owner`'s menu is open, for `Menu::open`.
    pub fn menu_open(&self, owner: MenuOwner) -> bool {
        self.open_menu().is_some_and(|m| m.owner == owner)
    }

    /// Open `owner`'s menu, at `at` for a context menu. A menu replaces
    /// whatever menu or select was open.
    pub fn show_menu(&mut self, owner: MenuOwner, at: Option<PointerPress>) {
        self.close_select();
        self.pointer.menu = None;
        self.pointer.tab_menu = false;
        self.kit.menu = None;
        match (owner, at) {
            (MenuOwner::Pane | MenuOwner::Tab, Some(at)) => {
                self.pointer.menu = Some(at);
                self.pointer.tab_menu = owner == MenuOwner::Tab;
            }
            _ => {
                self.kit.menu = Some(OpenMenu {
                    owner,
                    at,
                    submenu: None,
                });
            }
        }
    }

    /// The entry whose submenu is out, or none.
    pub fn set_submenu(&mut self, submenu: Option<usize>) {
        if self.open_menu().is_none() {
            return;
        }
        let owner = self.open_menu().map_or(MenuOwner::Pane, |m| m.owner);
        let menu = self.kit.menu.get_or_insert(OpenMenu {
            owner,
            at: None,
            submenu: None,
        });
        menu.submenu = submenu;
    }

    /// The open kit menu's surface, for paint order and input ownership.
    pub fn menu_surface(&self) -> Option<Surface> {
        self.open_menu().map(|m| Surface::Menu(m.owner))
    }

    /// The open kit select.
    pub const fn open_select(&self) -> Option<&OpenSelect> {
        self.kit.select.as_ref()
    }

    /// Whether `owner`'s select is open, for `Select::open`.
    pub fn select_open(&self, owner: SelectOwner) -> bool {
        self.open_select().is_some_and(|s| s.owner == owner)
    }

    /// The option the keyboard is on in `owner`'s open select.
    pub fn select_highlighted(&self, owner: SelectOwner) -> Option<&str> {
        let open = self.open_select().filter(|s| s.owner == owner)?;
        open.highlighted.as_deref()
    }

    /// The select's trigger was pressed: open it (closing any menu) or
    /// close it.
    pub fn toggle_select(&mut self, owner: SelectOwner) {
        if self.select_open(owner) {
            self.close_select();
            return;
        }
        self.close_menu();
        self.kit.select = Some(OpenSelect {
            owner,
            highlighted: None,
        });
    }

    /// Move the open select's keyboard highlight.
    pub fn highlight_option(&mut self, option: Option<&str>) {
        if let Some(open) = self.kit.select.as_mut() {
            open.highlighted = option.map(str::to_owned);
        }
    }

    /// Escape, a press outside, or a choice closes the select.
    pub fn close_select(&mut self) {
        self.kit.select = None;
    }

    /// The open kit select's surface.
    pub fn select_surface(&self) -> Option<Surface> {
        self.open_select().map(|s| Surface::Select(s.owner))
    }

    /// The toast region's surface while any toast shows: painted above the
    /// panes, never the keyboard's owner. The inbox's footer says the
    /// front one instead while the drawer is open.
    pub fn toasts_surface(&self) -> Option<Surface> {
        (!self.toasts().is_empty() && !self.inbox.open).then_some(Surface::Toasts)
    }

    /// The kit surfaces that are up, back to front: the toasts under any
    /// menu or select.
    pub fn kit_surfaces(&self) -> impl DoubleEndedIterator<Item = Surface> {
        [
            self.toasts_surface(),
            self.menu_surface(),
            self.select_surface(),
        ]
        .into_iter()
        .flatten()
    }

    /// The terminal font a select chose: one of `grid::FONT_CHOICES`, the
    /// empty default included. Anything else is ignored.
    pub fn choose_font(&mut self, family: &str) {
        self.close_select();
        if self.file_managed(self.file.font_family.is_some())
            || !grid::FONT_CHOICES.contains(&family)
            || family == self.font_family
        {
            return;
        }
        self.font_family = family.to_owned();
        let _ = self.prefs.set("font_family", &self.font_family);
        self.font_due = true;
    }

    /// The field `f` edits.
    pub fn field(&self, f: KitField) -> &TextEdit {
        match f {
            KitField::Palette => &self.palette_query,
            KitField::Find => &self.connection.find_query,
            KitField::SettingsSearch => &self.settings_ui.query,
            KitField::Rename => &self.connection.tabs.rename_field,
            KitField::InboxReply => &self.inbox.reply,
            KitField::Machine => &self.machines.field,
            KitField::PhoneSessions => self.phone.as_ref().map_or(&self.kit.empty, |p| &p.query),
        }
    }

    /// The field `f` edits, for a kit input's `on_edit`.
    pub fn field_mut(&mut self, f: KitField) -> &mut TextEdit {
        match f {
            KitField::Palette => &mut self.palette_query,
            KitField::Find => &mut self.connection.find_query,
            KitField::SettingsSearch => &mut self.settings_ui.query,
            KitField::Rename => &mut self.connection.tabs.rename_field,
            KitField::InboxReply => &mut self.inbox.reply,
            KitField::Machine => &mut self.machines.field,
            KitField::PhoneSessions => match self.phone.as_mut() {
                Some(phone) => &mut phone.query,
                None => {
                    self.kit.spare = TextEdit::default();
                    &mut self.kit.spare
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;

    fn desk() -> Desk {
        Desk::new(&Resolved::offline("kit"))
    }

    fn press() -> PointerPress {
        PointerPress { x: 10.0, y: 20.0 }
    }

    #[test]
    fn a_context_menu_is_the_open_kit_menu_at_its_point() {
        let mut desk = desk();
        assert_eq!(desk.open_menu(), None);
        desk.show_menu(MenuOwner::Tab, Some(press()));
        assert!(desk.pointer.tab_menu, "the tab's rows");
        assert!(desk.menu_open(MenuOwner::Tab));
        assert!(!desk.menu_open(MenuOwner::Pane));
        assert_eq!(desk.menu_surface(), Some(Surface::Menu(MenuOwner::Tab)));
        desk.close_menu();
        assert_eq!(desk.open_menu(), None);
    }

    #[test]
    fn a_menu_and_a_select_never_open_together() {
        let mut desk = desk();
        desk.toggle_select(SelectOwner::Font);
        assert!(desk.select_open(SelectOwner::Font));
        desk.show_menu(MenuOwner::Pane, Some(press()));
        assert!(!desk.select_open(SelectOwner::Font), "the menu closed it");
        desk.toggle_select(SelectOwner::Font);
        assert_eq!(desk.open_menu(), None, "the select closed the menu");
        desk.toggle_select(SelectOwner::Font);
        assert_eq!(desk.open_select(), None, "a second press closes it");
    }

    #[test]
    fn a_submenu_stays_out_until_the_menu_closes() {
        let mut desk = desk();
        desk.set_submenu(Some(2));
        assert_eq!(desk.open_menu(), None, "no menu, no submenu");
        desk.show_menu(MenuOwner::Sidebar, None);
        desk.set_submenu(Some(2));
        assert_eq!(desk.open_menu().and_then(|m| m.submenu), Some(2));
        desk.close_menu();
        assert_eq!(desk.open_menu(), None);
    }

    #[test]
    fn the_select_keeps_its_keyboard_highlight_while_open() {
        let mut desk = desk();
        desk.highlight_option(Some("Menlo"));
        assert_eq!(desk.select_highlighted(SelectOwner::Font), None);
        desk.toggle_select(SelectOwner::Font);
        desk.highlight_option(Some("Menlo"));
        assert_eq!(desk.select_highlighted(SelectOwner::Font), Some("Menlo"));
        desk.close_select();
        assert_eq!(desk.select_highlighted(SelectOwner::Font), None);
    }

    #[test]
    fn choosing_a_font_takes_only_a_listed_family_and_closes_the_select() {
        let mut desk = desk();
        desk.prefs = rune::prefs::Prefs::memory();
        desk.toggle_select(SelectOwner::Font);
        let family = grid::FONT_CHOICES[1];
        desk.choose_font(family);
        assert_eq!(desk.font_family, family);
        assert!(desk.font_due, "the mono slot follows");
        assert_eq!(desk.open_select(), None);
        desk.choose_font("Comic Sans");
        assert_eq!(desk.font_family, family, "not a choice");
    }

    #[test]
    fn toasts_paint_under_a_menu_and_hide_behind_the_inbox() {
        let mut desk = desk();
        assert_eq!(desk.kit_surfaces().count(), 0);
        desk.say(super::super::NoticeKind::Info, "Copied");
        desk.show_menu(MenuOwner::Pane, Some(press()));
        let up: Vec<_> = desk.kit_surfaces().collect();
        assert_eq!(up, [Surface::Toasts, Surface::Menu(MenuOwner::Pane)]);
        assert_eq!(Surface::Toasts.modal(), None, "never the keyboard's");
        desk.close_menu();
        desk.inbox.open = true;
        assert_eq!(desk.toasts_surface(), None, "the footer says it");
    }

    #[test]
    fn every_kit_field_reaches_its_edit_and_a_missing_one_takes_nothing() {
        let mut desk = desk();
        for f in KitField::ALL {
            desk.field_mut(f).insert("x");
        }
        assert_eq!(desk.field(KitField::Find).text, "x");
        assert_eq!(desk.palette_query.text, "x");
        assert_eq!(desk.field(KitField::PhoneSessions).text, "", "no phone");
        let keys: std::collections::HashSet<_> = KitField::ALL.map(KitField::key).into();
        assert_eq!(keys.len(), KitField::ALL.len(), "one key each");
    }
}
