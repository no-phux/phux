//! Native desktop application.
//!
//! `open` is the parent binary's entry. Kit initializes the platform; this
//! module owns windows, commands, and chrome. The window root is
//! [`gpui_kit::component::Root`], which re-exports [`gpui_kit::base::Root`].

mod actions;
mod color;
mod desk;
mod host;
mod keys;
mod organization;
mod overlays;
mod panes;
mod session;
mod settings_panel;
mod typed;
mod view;

use std::path::PathBuf;

use gpui::App;
use gpui_kit::WindowHandle;
use gpui_kit::component::Root;
use phux_protocol::ResourceId;

pub use actions::{Quit, Run};
pub use desk::{Desk, Modal, Toast, ToastKind};
pub use session::Desktop;

/// Options the parent binary fills in. This crate does not discover a
/// production socket or replace an installed app.
pub struct AppOptions {
    /// Explicit server socket. Never the production default unless the parent names it.
    pub socket: PathBuf,
    /// Home session name for this window.
    pub session: String,
    /// `phux` CLI used for session create/rename/close and server ensure.
    pub phux: PathBuf,
    /// Layout file. Empty selects the standard store for `socket` and `session`.
    pub layout_path: PathBuf,
    /// Secondary window: a new terminal, not the restored layout. Writes nothing.
    pub fresh: bool,
    /// Move-pane window: show this existing terminal instead of spawning.
    pub initial_terminal: Option<ResourceId>,
    /// Open the quick-terminal window instead of the main window.
    pub quick: bool,
    /// Shown in the window when the server could not be ensured. Not hidden as an empty inventory.
    pub startup_error: Option<String>,
}

/// Open the desktop on an already-initialized GPUI app.
///
/// Parent must call `gpui_kit::init` and `application().with_assets(assets::Assets)`
/// before this. The returned handle is `WindowHandle<gpui_kit::component::Root>`.
///
/// Activity is not polled. Each window waits on [`crate::connection::Connection::activity`]
/// and drains on that edge. Terminal entities are kept per placement until the
/// view id changes.
pub fn open(options: AppOptions, cx: &mut App) -> anyhow::Result<WindowHandle<Root>> {
    let desktop = Desktop::install(cx);
    desktop.update(cx, |desktop, cx| desktop.launch(options, cx))
}

/// Command ids the palette, menus, and key bindings dispatch through [`Run`].
pub fn command_ids() -> impl Iterator<Item = &'static str> {
    crate::settings::builtin_commands()
        .iter()
        .map(|command| command.id)
}
