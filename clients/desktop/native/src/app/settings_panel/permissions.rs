//! Permission setup opens the OS-owned approval surface. Never infer a grant
//! from a saved preference or read protected user data to probe for access.

use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{WeakEntity, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::setting::SettingField;
use gpui_kit::component::{Disableable, Sizable};

use super::catalog::{Content, Group, Item};
use super::fact;
use crate::app::desk::Desk;
use crate::launch::shell_path;

const FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
const NOTIFICATIONS: &str = "x-apple.systempreferences:com.apple.Notifications-Settings.extension";

pub(super) fn page(desk: &Desk, handle: &WeakEntity<Desk>) -> Content {
    let desktop = std::env::current_exe()
        .ok()
        .map(|path| desktop_target(&path));
    // Resolve symlinked launchers so Finder selects the executable, rather
    // than a Homebrew or user-created symlink that Settings may reject.
    // Automatic discovery keeps the command name `phux`; the recorded install
    // path is the file Finder should select.
    let launcher_path = desk
        .cli_install
        .as_ref()
        .map(|cli| cli.path.clone())
        .unwrap_or_else(|| desk.phux.clone());
    let launcher = launcher_path
        .canonicalize()
        .ok()
        .filter(|path| path.is_file());
    Content::new("Permissions")
        .icon(IconName::Lock)
        .group(
            Group::new()
                .title("Protected files")
                .description(
                    "Optional for work in protected folders. Set up opens Full Disk Access \
                     and selects the item in Finder. Drag it into the list and turn it on. \
                     macOS requires your approval.",
                )
                .item(file_access(
                    "Phux desktop",
                    "desktop",
                    desktop,
                    "Allows the desktop to read protected project files.",
                ))
                .item(file_access(
                    "Local shells & agents",
                    "server",
                    launcher,
                    "The local phux launcher runs shells and agents in a separate server. \
                     A server started by a background service may need its own grant. \
                     If that service uses a different phux install, add its executable instead.",
                ))
                .item(
                    fact("After granting access", "Reopen Phux".into()).description(
                        "An already-running background server may also need a restart. \
                         Finish or save its work first: quitting the desktop leaves shells running. \
                         These settings apply to this Mac; remote hosts have their own permissions.",
                    ),
                ),
        )
        .group(
            Group::new()
                .title("Notifications & other permissions")
                .item(
                    Item::new(
                        "System notifications",
                        SettingField::render(|_, _, _| {
                            Button::new("settings-permissions-notifications")
                                .small()
                                .label("Open Settings")
                                .on_click(|_, _, cx| cx.open_url(NOTIFICATIONS))
                        }),
                    )
                    .description("Choose Phux in macOS Notifications to allow background alerts."),
                )
                .item(fact("Terminal & quick terminal", "No extra grant needed".into()).description(
                    "Typing, clipboard and global quick-terminal shortcuts work without \
                     Accessibility, Input Monitoring or Screen Recording. Tools you run that \
                     control other apps or capture the screen may request separate permissions; \
                     Full Disk Access does not grant those.",
                )),
        )
        .group(command_line(desk, handle))
}

fn command_line(desk: &Desk, handle: &WeakEntity<Desk>) -> Group {
    let (description, ready) = match shell_path::profile_target() {
        Some((profile, _)) => (
            format!(
                "~/.local/bin is where Desktop installs the phux command. Terminal reads {}. \
                 Add that directory to PATH, or copy the command.",
                shell_path::home_display(&profile)
            ),
            true,
        ),
        None => (
            "HOME is not set, so Phux cannot update a shell profile.".to_owned(),
            false,
        ),
    };
    let detail = desk.path_prompt.message.clone().unwrap_or_else(|| {
        "Nothing is written until you choose Add to PATH. Then open a new Terminal window."
            .to_owned()
    });
    let add = handle.clone();
    let copy = handle.clone();
    Group::new()
        .title("Command line")
        .description(description)
        .item(
            Item::new(
                "Terminal PATH",
                SettingField::render(move |_, _, _| {
                    let add = add.clone();
                    let copy = copy.clone();
                    div()
                        .debug_selector(|| "settings-cli-path".into())
                        .flex()
                        .gap(px(8.))
                        .child(
                            Button::new("settings-cli-path-add")
                                .small()
                                .label("Add to PATH")
                                .disabled(!ready)
                                .on_click(move |_, _, cx| {
                                    let _ = add.update(cx, |desk, cx| desk.add_cli_to_path(cx));
                                }),
                        )
                        .child(
                            Button::new("settings-cli-path-copy")
                                .small()
                                .label("Copy command")
                                .disabled(!ready)
                                .on_click(move |_, _, cx| {
                                    let _ = copy.update(cx, |desk, cx| desk.copy_cli_command(cx));
                                }),
                        )
                }),
            )
            .description(detail)
            .keywords(["path", "terminal", "command", "zsh", "bashrc", "local bin"]),
        )
}

/// Reveal the running bundle even if it was renamed or installed outside
/// /Applications. Unbundled development builds reveal their executable.
fn desktop_target(executable: &Path) -> PathBuf {
    executable
        .ends_with("Contents/MacOS/phux-desktop")
        .then(|| executable.ancestors().nth(3))
        .flatten()
        .filter(|path| path.extension().is_some_and(|extension| extension == "app"))
        .unwrap_or(executable)
        .to_path_buf()
}

fn file_access(title: &'static str, id: &'static str, path: Option<PathBuf>, note: &str) -> Item {
    let description = path.as_ref().map_or_else(
        || format!("Executable unavailable. {note}"),
        |path| format!("{}\n{note}", path.display()),
    );
    Item::new(
        title,
        SettingField::render(move |_, _, _| {
            let path = path.clone();
            div()
                .debug_selector(move || format!("settings-permissions-{id}"))
                .flex()
                .gap(px(8.))
                .child(
                    Button::new(format!("settings-permissions-setup-{id}"))
                        .small()
                        .label("Set up")
                        .disabled(path.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(path) = &path {
                                cx.open_url(FULL_DISK_ACCESS);
                                cx.reveal_path(path);
                            }
                        }),
                )
        }),
    )
    .description(description)
    .keywords(["full disk access", "files folders", "drag drop", "privacy"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_setup_reveals_the_running_app_including_renamed_bundles() {
        for bundle in ["/Applications/Phux.app", "/Users/me/Apps/Phux Preview.app"] {
            assert_eq!(
                desktop_target(&Path::new(bundle).join("Contents/MacOS/phux-desktop")),
                Path::new(bundle),
            );
        }
        let development = Path::new("/checkout/target/debug/phux-desktop");
        assert_eq!(desktop_target(development), development);
    }
}
