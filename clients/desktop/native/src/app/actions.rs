//! One command action. Menus, key bindings, and controls all dispatch [`Run`].

use gpui_kit::Action;

gpui_kit::actions!(phux_desktop, [Quit, OpenPathDirectory]);

/// Run a desktop command by the same id the palette, menus, and key bindings use.
#[derive(Action, Clone, PartialEq, Eq, Debug)]
#[action(namespace = phux_desktop, no_json)]
pub struct Run {
    /// Catalog id, or a Ghostty payload id (`send:`, `write-file:`, …).
    pub id: String,
}

impl Run {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

/// The catalog title for command `id`, so every menu names a command the way
/// the palette does.
///
/// # Panics
///
/// When `id` is not catalogued; the menu tests resolve every id menus name.
#[must_use]
pub fn title(id: &str) -> &'static str {
    crate::settings::builtin_commands()
        .iter()
        .find(|command| command.id == id)
        .map(|command| command.title)
        .unwrap_or_else(|| panic!("{id} is not a catalogued command"))
}
