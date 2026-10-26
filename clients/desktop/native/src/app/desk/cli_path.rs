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
    pub(super) fn start_path_probe(&mut self, cx: &mut Context<Self>) {
        if shell_path::is_dismissed() {
            self.path_prompt.dismissed = true;
            return;
        }
        let Some(cli) = user_local_cli(self) else {
            return;
        };
        let Some(shell) = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
        else {
            self.path_prompt.status = Status::Unverified;
            return;
        };
        self.path_prompt.status = Status::Pending;
        self.path_prompt.task = Some(cx.spawn(async move |this, cx| {
            let status = smol::unblock(move || {
                let result = shell_path::probe_login_shell(&shell, PROBE_TIMEOUT);
                match shell_path::classify(result, &cli) {
                    PathStatus::OnPath => Status::OnPath,
                    PathStatus::Missing => Status::Missing,
                    PathStatus::Unverified => Status::Unverified,
                }
            })
            .await;
            let _ = this.update(cx, |desk, cx| {
                desk.path_prompt.status = status;
                cx.notify();
            });
        }));
    }

    pub(crate) fn add_cli_to_path(&mut self, cx: &mut Context<Self>) {
        let Some((profile, line)) = shell_path::profile_target() else {
            self.path_prompt.message =
                Some("HOME is not set, so the shell profile was not changed.".into());
            cx.notify();
            return;
        };
        let shown = display_profile(&profile);
        match shell_path::append_once(&profile, shell_path::INSTALLER_MARKER, &line) {
            Ok(true) => {
                self.path_prompt.added = true;
                self.path_prompt.message = Some(format!(
                    "Added ~/.local/bin to PATH in {shown}. Open a new Terminal window to use \
                     phux. Windows already open won't see it."
                ));
            }
            Ok(false) => {
                self.path_prompt.added = true;
                self.path_prompt.message = Some(format!(
                    "{shown} already adds ~/.local/bin to PATH. Open a new Terminal window. If \
                     phux is still not found, check that nothing later in {shown} resets PATH."
                ));
            }
            Err(error) => {
                self.path_prompt.message = Some(format!(
                    "{error:#}. Nothing was changed. Use Copy line and add it to {shown} yourself."
                ));
            }
        }
        cx.notify();
    }

    pub(crate) fn copy_cli_command(&mut self, cx: &mut Context<Self>) {
        let Some((profile, line)) = shell_path::profile_target() else {
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

fn user_local_cli(desk: &Desk) -> Option<PathBuf> {
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
        return format!(
            "Phux could not check whether Terminal finds phux, so it won't change {profile}. To \
             add it yourself, paste the line below at the end of {profile}, then open a new \
             Terminal window."
        );
    }
    let lead = if desk.cli_install.as_ref().is_some_and(|cli| cli.installed) {
        "Phux installed the phux command in ~/.local/bin."
    } else {
        "The phux command is in ~/.local/bin."
    };
    format!(
        "{lead} Terminal can't find it yet. Add to PATH appends the line below to {profile}. \
         Then open a new Terminal window."
    )
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
    if !matches!(
        desk.path_prompt.status,
        Status::Missing | Status::Unverified
    ) {
        return None;
    }
    let (profile, line) = shell_path::profile_target()?;
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
