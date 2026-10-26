//! One quiet corner card when Terminal cannot see `~/.local/bin`.
//! It never takes focus. A profile is written only after Add to PATH.

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, ClipboardItem, Context, FontWeight, MouseButton, Task, Window, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Icon, Sizable};

use super::Desk;
use crate::app::color::from_hex;
use crate::app::desk::Modal;
use crate::app::overlays::{SHEET_GUTTER, corner};
use crate::app::skin::Look;
use crate::app::ui::ghost_icon;
use crate::launch::shell_path::{self, PathStatus};

const TITLE: &str = "Use phux in Terminal";
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(crate) struct Prompt {
    pub status: Status,
    pub dismissed: bool,
    pub added: bool,
    pub message: Option<String>,
    /// Login-shell environment from a probe that succeeded. `None` means the
    /// probe has not run or it failed, so profile lookup uses this process.
    pub login: Option<shell_path::LoginEnv>,
    task: Option<Task<()>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Status {
    #[default]
    Idle,
    Pending,
    OnPath,
    Missing,
    Unverified,
}

impl Desk {
    /// The call site is macOS-only. The body stays compiled everywhere so a
    /// Finder-launch regression is not Linux-only dead code.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn start_path_probe(&mut self, cx: &mut Context<Self>) {
        // Dismissal hides the card only; Settings still needs the probe result.
        self.path_prompt.dismissed = shell_path::is_dismissed();
        let cli = user_local_cli(self);
        let shell = std::env::var_os("SHELL").map(PathBuf::from);
        let Some((cli, shell)) = schedule_path_probe(&mut self.path_prompt, cli, shell) else {
            return;
        };
        self.path_prompt.task = Some(cx.spawn(async move |this, cx| {
            let probed =
                smol::unblock(move || shell_path::probe_login_shell(&shell, PROBE_TIMEOUT)).await;
            let _ = this.update(cx, |desk, cx| {
                let command = match &probed {
                    Ok(probe) => {
                        desk.path_prompt.login = Some(probe.env.clone());
                        Ok(probe.command.clone())
                    }
                    Err(error) => Err(anyhow::anyhow!("{error:#}")),
                };
                desk.path_prompt.status = status_of(command, &cli);
                cx.notify();
            });
        }));
    }

    pub(crate) fn add_cli_to_path(&mut self, cx: &mut Context<Self>) {
        let Some((profile, line)) = shell_path::profile_target(self.path_prompt.login.as_ref())
        else {
            self.path_prompt.message =
                Some("HOME is not set, so the shell profile was not changed.".into());
            cx.notify();
            return;
        };
        let shown = display_profile(&profile);
        let written = match self.path_prompt.login.clone() {
            Some(env) => shell_path::append_guarded(
                &profile,
                shell_path::INSTALLER_MARKER,
                &line,
                shell_path::changes_allowed(env.modify_path.as_deref()),
            ),
            None => shell_path::append_once(&profile, shell_path::INSTALLER_MARKER, &line),
        };
        match written {
            Ok(true) => {
                self.path_prompt.added = true;
                self.reprobe_after_add(shown, cx);
            }
            Ok(false) => {
                self.path_prompt.added = true;
                self.path_prompt.message = Some(already_message(&shown));
            }
            Err(error) => {
                self.path_prompt.message = Some(add_error_message(&format!("{error:#}"), &shown));
            }
        }
        cx.notify();
    }

    fn reprobe_after_add(&mut self, shown: String, cx: &mut Context<Self>) {
        let cli = user_local_cli(self);
        let shell = std::env::var_os("SHELL").map(PathBuf::from);
        self.path_prompt.task = Some(cx.spawn(async move |this, cx| {
            let probed = smol::unblock(move || {
                let shell = shell.filter(|path| !path.as_os_str().is_empty())?;
                shell_path::probe_login_shell(&shell, PROBE_TIMEOUT).ok()
            })
            .await;
            let _ = this.update(cx, |desk, cx| {
                let on_path = if let Some(probe) = &probed {
                    desk.path_prompt.login = Some(probe.env.clone());
                    cli.as_ref().is_some_and(|cli| {
                        let status = status_of(Ok(probe.command.clone()), cli);
                        desk.path_prompt.status = status;
                        status == Status::OnPath
                    })
                } else {
                    false
                };
                desk.path_prompt.message = Some(added_outcome(on_path, &shown));
                cx.notify();
            });
        }));
    }

    pub(crate) fn copy_cli_command(&mut self, cx: &mut Context<Self>) {
        let Some((profile, line)) = shell_path::profile_target(self.path_prompt.login.as_ref())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(line));
        let shown = display_profile(&profile);
        self.path_prompt.message = Some(format!(
            "Copied. Paste it at the end of {shown}, then open a new Terminal window."
        ));
        cx.notify();
    }

    fn dismiss_cli_path(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = shell_path::dismiss() {
            self.path_prompt.message = Some(format!("{error:#}"));
        } else {
            self.path_prompt.dismissed = true;
        }
        cx.notify();
    }
}

pub(crate) fn user_local_cli(desk: &Desk) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let path = desk
        .cli_install
        .as_ref()
        .map(|cli| cli.path.clone())
        .unwrap_or_else(|| desk.phux.clone());
    shell_path::is_user_local_cli(&home, &path).then_some(path)
}

fn display_profile(path: &std::path::Path) -> String {
    shell_path::home_display(path)
}

fn body(desk: &Desk, profile: &std::path::Path) -> String {
    if let Some(message) = &desk.path_prompt.message {
        return message.clone();
    }
    let profile = display_profile(profile);
    if desk.path_prompt.status == Status::Unverified {
        return unverified_body(&profile);
    }
    missing_body(
        desk.cli_install.as_ref().is_some_and(|cli| cli.installed),
        &profile,
    )
}

/// Continue the login-shell probe after dismissal is recorded. `None` means
/// there is no user-local CLI, or the shell could not be named.
pub(crate) fn schedule_path_probe(
    prompt: &mut Prompt,
    cli: Option<PathBuf>,
    shell: Option<PathBuf>,
) -> Option<(PathBuf, PathBuf)> {
    let cli = cli?;
    let Some(shell) = shell.filter(|path| !path.as_os_str().is_empty()) else {
        prompt.status = Status::Unverified;
        return None;
    };
    prompt.status = Status::Pending;
    Some((cli, shell))
}

pub(crate) fn status_of(result: anyhow::Result<Option<PathBuf>>, cli: &std::path::Path) -> Status {
    match shell_path::classify(result, cli) {
        PathStatus::OnPath => Status::OnPath,
        PathStatus::Missing => Status::Missing,
        PathStatus::Unverified => Status::Unverified,
    }
}

pub(crate) fn missing_body(installed: bool, profile: &str) -> String {
    let lead = if installed {
        "Phux installed the phux command in ~/.local/bin."
    } else {
        "The phux command is in ~/.local/bin."
    };
    format!(
        "{lead} Terminal can't find it yet. Add to PATH appends the line below to {profile}. \
         Then open a new Terminal window."
    )
}

pub(crate) fn unverified_body(profile: &str) -> String {
    format!(
        "Phux could not check whether Terminal finds phux, so it won't change {profile}. To add \
         it yourself, paste the line below at the end of {profile}, then open a new Terminal \
         window."
    )
}

pub(crate) fn added_message(shown: &str) -> String {
    format!(
        "Added ~/.local/bin to PATH in {shown}. Open a new Terminal window to use phux. Windows \
         already open won't see it."
    )
}

pub(crate) fn already_message(shown: &str) -> String {
    format!(
        "{shown} already adds ~/.local/bin to PATH. Open a new Terminal window. If phux is still \
         not found, check that nothing later in {shown} resets PATH."
    )
}

pub(crate) fn add_error_message(error: &str, shown: &str) -> String {
    format!("{error}. Nothing was changed. Use Copy line and add it to {shown} yourself.")
}

/// Success only when the login shell we just wrote for can see `phux`.
pub(crate) fn added_outcome(on_path: bool, shown: &str) -> String {
    if on_path {
        added_message(shown)
    } else {
        format!(
            "Added ~/.local/bin to PATH in {shown}, but your shell isn't picking it up. Open a \
             new Terminal window. If phux is still not found, check ZDOTDIR or that nothing \
             later in {shown} resets PATH."
        )
    }
}

pub(crate) fn card(
    desk: &Desk,
    look: &Look,
    window: &Window,
    cx: &mut Context<Desk>,
) -> Option<AnyElement> {
    if !matches!(desk.modal, Modal::None) || desk.path_prompt.dismissed {
        return None;
    }
    let reported = desk.path_prompt.added && desk.path_prompt.message.is_some();
    if !reported
        && !matches!(
            desk.path_prompt.status,
            Status::Missing | Status::Unverified
        )
    {
        return None;
    }
    let (profile, line) = shell_path::profile_target(desk.path_prompt.login.as_ref())?;
    let allow_add = desk.path_prompt.status == Status::Missing && !desk.path_prompt.added;
    let top = crate::app::view::content_top(desk, &look.skin) + SHEET_GUTTER / 2.;
    let shift = {
        let updater = desk.updates.read(cx);
        if updater.card_expanded && updater.has_card() {
            280.
        } else {
            0.
        }
    };
    let width = (window.viewport_size().width.as_f32() - SHEET_GUTTER * 2.).min(344.);
    let content = crate::app::overlays::card(look, width)
        .id("cli-path-card")
        .debug_selector(|| "cli-path-card".into())
        .max_h(window.viewport_size().height - px(top + shift + SHEET_GUTTER))
        .overflow_y_scroll()
        .p(px(16.))
        .rounded(corner(look, 14.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_start()
                .gap(px(12.))
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(38.))
                        .rounded_full()
                        .border_2()
                        .border_color(look.ink.opacity(0.15))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(IconName::Terminal)
                                .size(px(16.))
                                .text_color(look.ink),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(TITLE),
                        )
                        .child(
                            div()
                                .text_xs()
                                .line_height(px(17.))
                                .text_color(from_hex(&look.chrome.subtext))
                                .child(body(desk, &profile)),
                        )
                        .child(
                            div()
                                .mt(px(4.))
                                .text_xs()
                                .line_height(px(17.))
                                .text_color(look.ink)
                                .child(line.clone()),
                        ),
                )
                .child(
                    div().debug_selector(|| "cli-path-dismiss".into()).child(
                        ghost_icon("cli-path-dismiss", IconName::X, "Dismiss")
                            .on_click(cx.listener(|desk, _, _, cx| desk.dismiss_cli_path(cx))),
                    ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .child(
                    Button::new("cli-path-copy")
                        .label("Copy line")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|desk, _, _, cx| desk.copy_cli_command(cx))),
                )
                .when(allow_add, |row| {
                    row.child(
                        Button::new("cli-path-add")
                            .label("Add to PATH")
                            .primary()
                            .small()
                            .on_click(cx.listener(|desk, _, _, cx| desk.add_cli_to_path(cx))),
                    )
                }),
        );
    Some(
        div()
            .absolute()
            .top(px(top + shift))
            .right(px(SHEET_GUTTER))
            .child(content)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn card_copy_pins_the_bodies_and_the_three_results() {
        let profile = "~/.zshrc";
        assert_eq!(
            missing_body(true, profile),
            "Phux installed the phux command in ~/.local/bin. Terminal can't find it yet. Add to \
             PATH appends the line below to ~/.zshrc. Then open a new Terminal window."
        );
        assert_eq!(
            unverified_body(profile),
            "Phux could not check whether Terminal finds phux, so it won't change ~/.zshrc. To \
             add it yourself, paste the line below at the end of ~/.zshrc, then open a new \
             Terminal window."
        );
        assert_eq!(
            added_message(profile),
            "Added ~/.local/bin to PATH in ~/.zshrc. Open a new Terminal window to use phux. \
             Windows already open won't see it."
        );
        assert_eq!(
            already_message(profile),
            "~/.zshrc already adds ~/.local/bin to PATH. Open a new Terminal window. If phux is \
             still not found, check that nothing later in ~/.zshrc resets PATH."
        );
        assert_eq!(
            add_error_message("PHUX_NO_MODIFY_PATH is set", profile),
            "PHUX_NO_MODIFY_PATH is set. Nothing was changed. Use Copy line and add it to \
             ~/.zshrc yourself."
        );
        assert_eq!(added_outcome(true, profile), added_message(profile));
        assert_eq!(
            added_outcome(false, profile),
            "Added ~/.local/bin to PATH in ~/.zshrc, but your shell isn't picking it up. Open a \
             new Terminal window. If phux is still not found, check ZDOTDIR or that nothing \
             later in ~/.zshrc resets PATH."
        );
    }

    /// Dismissal used to return before the probe, so Settings never learned
    /// that `phux` was already on PATH.
    #[cfg(unix)]
    #[test]
    fn a_dismissed_card_still_sets_on_path() {
        let root = std::env::temp_dir().join(format!(
            "phux-cli-path-dismissed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let cli = root.join("bin/phux");
        std::fs::write(&cli, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        let shell = root.join("stub-shell");
        let bin = cli.parent().unwrap().display();
        std::fs::write(
            &shell,
            format!(
                "#!/bin/sh\nprintf '%s\\n' 'welcome from rc'\nPATH={bin}:/usr/bin:/bin\nexport PATH\nif [ \"$1\" = -lic ]; then /bin/sh -c \"$2\"; fi\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut prompt = Prompt {
            dismissed: true,
            ..Prompt::default()
        };
        let scheduled = schedule_path_probe(&mut prompt, Some(cli.clone()), Some(shell.clone()));
        let (cli, shell) = scheduled.expect("dismissal skipped the probe");
        prompt.status = status_of(
            shell_path::probe_login_shell(&shell, Duration::from_secs(3))
                .map(|probe| probe.command),
            &cli,
        );
        assert!(prompt.dismissed);
        assert_eq!(prompt.status, Status::OnPath);
        let _ = std::fs::remove_dir_all(root);
    }
}
