//! Native desktop application.
//!
//! `open` is the parent binary's entry. Kit initializes the platform; this
//! module owns windows, commands, and chrome. The window root is
//! [`gpui_kit::component::Root`], which re-exports [`gpui_kit::base::Root`].

mod actions;
mod approvals;
pub(crate) mod branding;
mod capture;
mod changes;
mod color;
mod conversation;
mod desk;
mod diagnostics;
mod fleet;
mod host;
mod hud;
mod keys;
mod machine_name;
mod menus;
mod motion;
mod native_launch;
mod notifications;
mod organization;
mod organize;
mod overlays;
mod pane_identity;
mod panes;
mod performance;
mod session;
mod settings_panel;
mod skin;
mod tab_peek;
mod typed;
mod ui;
mod updates;
mod view;

use std::path::PathBuf;

use gpui::App;
use gpui_kit::WindowHandle;
use gpui_kit::component::Root;
use phux_protocol::ResourceId;

pub use actions::{Quit, Run};
pub use desk::{Desk, Modal};
pub use notifications::{Feed, Kind, Notification};
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
    /// Open the native performance monitor at launch; it can also be opened live.
    pub dev_telemetry: bool,
    /// Shown in the window when the server could not be ensured. Not hidden as an empty inventory.
    pub startup_error: Option<String>,
}

/// Open the desktop on an already-initialized GPUI app.
///
/// Parent must call `gpui_kit::init` and `application().with_assets(crate::assets::Assets)`
/// before this. The returned handle is `WindowHandle<gpui_kit::component::Root>`.
///
/// Activity is not polled. Each window waits on [`crate::connection::Connection::activity`]
/// and drains on that edge. Terminal entities are kept per placement until the
/// view id changes.
pub fn open(options: AppOptions, cx: &mut App) -> anyhow::Result<WindowHandle<Root>> {
    open_with_restore(options, false, cx)
}

/// The installed entry point opts into a pending update's one-time handoff.
pub(crate) fn open_with_restore(
    options: AppOptions,
    restore: bool,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Root>> {
    let desktop = Desktop::install(cx);
    desktop.update(cx, |desktop, cx| {
        desktop.launch_with_restore(options, restore, cx)
    })
}

/// Open the real menu material in a standalone native fixture.
#[cfg(all(feature = "ui-fixtures", target_os = "macos"))]
pub fn open_menu_fixture(cx: &mut App) -> anyhow::Result<()> {
    ui::typography::install(cx);
    ui::menu::preview(cx)
}

/// Preview or capture the real monitor without a daemon or production state.
#[cfg(all(feature = "ui-fixtures", target_os = "macos"))]
pub fn preview_performance_monitor(capture: Option<PathBuf>, cx: &mut App) -> anyhow::Result<()> {
    ui::typography::install(cx);
    performance::preview(capture, cx)
}

/// Open a desk and capture every Settings page into `directory`, then quit.
#[cfg(all(feature = "ui-fixtures", target_os = "macos"))]
pub fn capture_settings_fixture(
    options: AppOptions,
    directory: PathBuf,
    cx: &mut App,
) -> anyhow::Result<()> {
    let root = open(options, cx)?;
    settings_panel::preview::capture(root, directory, cx)
}

/// Capture the real fleet composition with isolated, representative inventory.
#[cfg(all(feature = "ui-fixtures", target_os = "macos"))]
pub fn capture_fleet_fixture(
    options: AppOptions,
    directory: PathBuf,
    cx: &mut App,
) -> anyhow::Result<()> {
    let root = open(options, cx)?;
    fleet::dashboard::preview::capture(root, directory, cx)
}

/// Capture the real desktop layouts with an embedded, server-free runtime.
#[cfg(all(feature = "ui-fixtures", target_os = "macos"))]
pub fn capture_design_fixture(
    options: AppOptions,
    directory: PathBuf,
    cx: &mut App,
) -> anyhow::Result<()> {
    view::preview::open(options, directory, cx)
}

/// Measure native menu creation without opening visible windows.
#[cfg(all(feature = "ui-fixtures", target_os = "macos"))]
pub fn benchmark_menu_fixture(cx: &mut App) -> anyhow::Result<()> {
    ui::menu::benchmark(cx)
}

/// Command ids the palette, menus, and key bindings dispatch through [`Run`].
pub fn command_ids() -> impl Iterator<Item = &'static str> {
    crate::settings::builtin_commands()
        .iter()
        .map(|command| command.id)
}

#[cfg(all(feature = "ui-fixtures", target_os = "macos"))]
pub use branding::verify_runtime_branding;
