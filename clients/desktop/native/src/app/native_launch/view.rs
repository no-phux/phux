//! Local UI over executing-host discovery and explicit onboarding actions.
use super::*;
use crate::app::color::from_hex;
use crate::app::desk::display_title;
use crate::app::overlays::{card, corner};
use crate::app::skin::Look;
use crate::app::ui::{ghost_button, muted};
use gpui::prelude::*;
use gpui::{AnyElement, SharedString, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::collapsible::Collapsible;

fn integration_row(
    item: &NativeIntegration,
    desk: &Desk,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let integration = item.integration.clone();
    let selector = format!("native-launch-{}", item.integration);
    let title = display_title(item.display_name.as_deref().unwrap_or(&item.integration));
    let launching = desk
        .native_launch
        .launch
        .as_ref()
        .is_some_and(|launch| launch.integration == item.integration);
    let disabled = desk.native_launch.busy()
        || !item.native_available
        || desk.native_launch.destination.is_none();
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(10.))
        .rounded(corner(look, 8.))
        .bg(from_hex(&look.chrome.theme.background))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    div().debug_selector(move || selector.clone()).child(
                        Button::new(SharedString::from(format!(
                            "launch-native-{}",
                            item.integration
                        )))
                        .label(if launching {
                            "Starting…"
                        } else {
                            "Start agent"
                        })
                        .small()
                        .w(px(100.))
                        .primary()
                        .disabled(disabled)
                        .on_click(cx.listener(
                            move |desk, _, window, cx| {
                                desk.launch_native_integration(&integration, window, cx)
                            },
                        )),
                    ),
                ),
        )
        .children(
            item.native_detail
                .as_ref()
                .map(|detail| muted(look, detail.clone())),
        )
        .when(item.integration == "opencode", |row| {
            row.child(setup_disclosure(item, desk, look, cx))
        })
        .into_any_element()
}

fn setup_disclosure(
    item: &NativeIntegration,
    desk: &Desk,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let open = desk
        .native_launch
        .setup_open
        .unwrap_or(!item.native_available);
    let trigger = ghost_button(
        format!("native-setup-{}", item.integration),
        "Setup & sign-in",
    )
    .icon(if open {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .xsmall()
    .toggled(open)
    .on_click(cx.listener(move |desk, _, _, cx| {
        desk.native_launch.setup_open = Some(!open);
        cx.notify();
    }));
    Collapsible::new()
        .open(open)
        .child(
            div()
                .debug_selector(|| "native-setup-toggle".into())
                .child(trigger),
        )
        .content(opencode_setup(item, desk, look, cx))
        .into_any_element()
}

fn opencode_setup(
    item: &NativeIntegration,
    desk: &Desk,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    div().flex().flex_col().gap(px(6.)).pt(px(6.))
        .child(muted(look, "Your OpenCode models, agents, skills and project settings. No remote TUI."))
        .when(!item.native_available, |row| row.child(muted(look, "Install OpenCode on this host using its official instructions, then Refresh. phux does not install it or copy credentials.")))
        .child(div().flex().flex_wrap().gap(px(8.))
            .child(ghost_button("opencode-install-guide", "Installation guide")
                .on_click(|_, _, cx| cx.open_url("https://opencode.ai/v2/docs/")))
            .when(item.auth_command.is_some(), |row| row.child(div().debug_selector(|| "opencode-sign-in".into()).child(ghost_button("opencode-auth-login", "Sign in on this host…")
                .disabled(desk.native_launch.busy() || !item.native_available || desk.native_launch.destination.is_none())
                .on_click(cx.listener(|desk, _, window, cx| desk.login_opencode(window, cx)))))))
        .when(item.native_available && item.auth_command.is_none(), |row| row.child(muted(look, "This custom profile controls its own authentication. Sign in through its documented workflow on this host before starting.")))
        .child(muted(look, "Sign-in opens OpenCode's own login in a terminal on this host. Credentials stay there."))
        .into_any_element()
}

pub(crate) fn panel(
    desk: &Desk,
    look: &Look,
    window: &gpui::Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let state = &desk.native_launch;
    let host = desk.remote.as_deref().unwrap_or("This machine");
    let destination = state.destination.as_ref().map(|target| {
        format!(
            "{} · {}\n{}",
            display_title(host),
            display_title(&target.name),
            display_title(&target.cwd)
        )
    });
    let context_error = desk.confirmed_native_destination().err();
    let rows = state
        .integrations
        .iter()
        .map(|item| integration_row(item, desk, look, cx))
        .collect::<Vec<_>>();
    card(look, 600.)
        .debug_selector(|| "native-launch-panel".into())
        .max_w_full()
        .max_h((window.viewport_size().height - px(32.)).min(px(680.)))
        .p(px(18.))
        .rounded(corner(look, 12.))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(div().text_lg().child("New Agent"))
        .child(muted(
            look,
            "Headless agent on the selected host. Native conversation here.",
        ))
        .children(destination.map(|text| div().text_sm().child(text)))
        .children(context_error.map(|error| muted(look, error)))
        .child(
            div()
                .id("native-launch-list")
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(8.))
                .when(
                    rows.is_empty() && state.discovery.is_none() && state.error.is_none(),
                    |list| {
                        list.child(
                            muted(look, "No native agents available on this host.").py(px(16.)),
                        )
                    },
                )
                .children(rows),
        )
        .when(state.discovery.is_some(), |panel| {
            panel.child("Checking agents on this host…")
        })
        .children(state.error.as_ref().map(|error| {
            div()
                .text_sm()
                .text_color(from_hex(&look.chrome.theme.warning))
                .child(error.clone())
        }))
        .child(muted(
            look,
            "Available does not mean signed in. The provider checks readiness when started.",
        ))
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(
                    ghost_button("native-launch-refresh", "Refresh")
                        .disabled(state.busy())
                        .on_click(
                            cx.listener(|desk, _, _, cx| desk.refresh_native_integrations(cx)),
                        ),
                )
                .child(
                    ghost_button("native-launch-close", "Close")
                        .on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))),
                ),
        )
        .into_any_element()
}
