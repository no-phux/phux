//! The workspace with no tab: start actions, or why there is no connection.

use gpui::{Context, IntoElement, ParentElement, Styled, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::kbd::Kbd;
use phux_client_runtime::control::Status;

use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::skin::Look;
use crate::app::ui::{keystroke, muted};

pub(super) fn empty_state(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.link.as_ref().map(|link| link.connection.status);
    let error = desk.connect_error.clone().or_else(|| {
        desk.link
            .as_ref()
            .and_then(|link| link.connection.error.clone())
    });
    let pending = desk
        .link
        .as_ref()
        .is_some_and(|link| link.workspace.pending() > 0);
    let new = if pending {
        "Starting terminal…"
    } else {
        "New terminal"
    };
    let body = match status {
        Some(Status::Attached) => div().flex().flex_col().items_center().gap(px(8.)).children(
            [
                ("empty-new", new, "new", true),
                ("empty-folder", "Open folder…", "folder", false),
                ("empty-palette", "Command palette", "palette", false),
            ]
            .map(|(id, label, command, primary)| {
                empty_action(desk, id, label, command, primary, cx)
            }),
        ),
        Some(Status::Idle | Status::Connecting | Status::Negotiated) => muted(
            look,
            match desk.remote.as_deref() {
                Some(host) => format!("Connecting to {host}…"),
                None => format!("Connecting to {}…", desk.socket.display()),
            },
        )
        .text_sm(),
        _ => div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .max_w(px(520.))
                    .text_center()
                    .text_sm()
                    .text_color(from_hex(&look.chrome.theme.danger))
                    .child(error.unwrap_or_else(|| "The connection closed.".into())),
            )
            .child(empty_action(
                desk,
                "empty-reconnect",
                "Reconnect",
                "reconnect",
                true,
                cx,
            )),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(18.))
        .child(
            div()
                .text_size(px(34.))
                .font_weight(gpui::FontWeight::EXTRA_BOLD)
                .child("phux"),
        )
        .child(body)
}

fn empty_action(
    desk: &Desk,
    id: &'static str,
    label: &str,
    command: &'static str,
    primary: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    let kbd = command_kbd(desk, command);
    let button = Button::new(id)
        .label(label.to_owned())
        .w(px(240.))
        .on_click(cx.listener(move |desk, _, window, cx| desk.execute(command, window, cx)));
    let button = if primary { button.primary() } else { button };
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(button)
        .child(div().w(px(64.)).children(kbd))
}

fn command_kbd(desk: &Desk, id: &str) -> Option<Kbd> {
    keystroke(desk.bound_chord(id)?).map(Kbd::new)
}
