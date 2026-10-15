//! One command action. Menus, key bindings, and controls all dispatch [`Run`].

use gpui_kit::Action;

gpui_kit::actions!(phux_desktop, [Quit, OpenPathDirectory, PerformanceMonitor]);

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
