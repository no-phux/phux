//! Small dialogs: rename, session, add host, and destructive confirmations.

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, Window, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;

use super::{InputSlot, sheet};
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::skin::Look;
use crate::app::ui::ghost_button;
use crate::settings::theme::Palette;

pub(super) fn dialog_card(title: &str, colors: &Palette, look: &Look) -> gpui::Div {
    sheet(look, 440.)
        .p(px(24.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(
            div()
                .pb(px(4.))
                .text_size(px(18.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(from_hex(&colors.theme.foreground))
                .child(title.to_owned()),
        )
}

pub(super) fn dialog_buttons(confirm: Button, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .pt(px(8.))
        .flex()
        .justify_end()
        .gap(px(8.))
        .child(
            Button::new("dialog-cancel")
                .label("Cancel")
                .ghost()
                .on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))),
        )
        .child(confirm)
}

pub(super) fn rename_dialog(
    desk: &Desk,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(input) = desk.input_entity(InputSlot::Rename) else {
        return div().into_any_element();
    };
    dialog_card("Rename Tab", colors, look)
        .child(
            div()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child("Leave it blank to follow the terminal title."),
        )
        .child(Input::new(&input))
        .child(dialog_buttons(
            Button::new("rename-ok")
                .label("Rename")
                .primary()
                .on_click(cx.listener(|desk, _, window, cx| {
                    let title = desk.input_value(InputSlot::Rename, cx);
                    desk.apply_rename(title, window, cx);
                })),
            cx,
        ))
        .into_any_element()
}

pub(super) fn session_dialog(
    desk: &Desk,
    create: bool,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(name) = desk.input_entity(InputSlot::SessionName) else {
        return div().into_any_element();
    };
    let directory = desk.input_entity(InputSlot::SessionDir);
    dialog_card(
        if create {
            "New Session"
        } else {
            "Rename Session"
        },
        colors,
        look,
    )
    .child(Input::new(&name))
    .children(directory.map(|directory| Input::new(&directory)))
    .child(dialog_buttons(
        Button::new("session-ok")
            .label(if create { "Create" } else { "Rename" })
            .primary()
            .on_click(cx.listener(|desk, _, window, cx| {
                let name = desk.input_value(InputSlot::SessionName, cx);
                let directory = desk
                    .input_entity(InputSlot::SessionDir)
                    .map(|input| input.read(cx).value().to_string());
                desk.apply_session(name, directory, window, cx);
            })),
        cx,
    ))
    .into_any_element()
}

pub(super) fn add_host_dialog(
    desk: &Desk,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let (Some(target), Some(name)) = (
        desk.input_entity(InputSlot::HostTarget),
        desk.input_entity(InputSlot::HostName),
    ) else {
        return div().into_any_element();
    };
    dialog_card("Add Host", colors, look)
        .child(
            div()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child("Uses the ssh access you already have. phux must be installed on the host; enrollment starts its server, pairs, and finds a direct route."),
        )
        .child(Input::new(&target))
        .child(Input::new(&name))
        .child(dialog_buttons(
            Button::new("host-add-ok")
                .label("Add Host")
                .primary()
                .on_click(cx.listener(|desk, _, window, cx| {
                    let target = desk.input_value(InputSlot::HostTarget, cx);
                    let name = desk.input_value(InputSlot::HostName, cx);
                    desk.apply_add_host(target, name, window, cx);
                })),
            cx,
        ))
        .into_any_element()
}

/// Link a session into a project: type a name, or pick one already in use
/// on any machine. A linked session can be unlinked back to its repository.
pub(super) fn project_dialog(
    desk: &Desk,
    session: &str,
    linked: bool,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(input) = desk.input_entity(InputSlot::Project) else {
        return div().into_any_element();
    };
    let current = desk.input_value(InputSlot::Project, cx);
    let mut known: Vec<String> = desk
        .work_catalog()
        .into_iter()
        .filter_map(|work| work.project.map(|(_, name)| name))
        .filter(|name| *name != current.trim())
        .collect();
    known.sort();
    known.dedup();
    let choices = (!known.is_empty()).then(|| {
        div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .children(known.into_iter().enumerate().map(|(index, name)| {
                let label = name.clone();
                ghost_button(("project-choice", index), label).on_click(cx.listener(
                    move |desk, _, window, cx| desk.link_project(Some(name.clone()), window, cx),
                ))
            }))
    });
    dialog_card(&format!("Link {session} to a Project"), colors, look)
        .child(
            div()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child("A project's sessions are listed together, from every machine. Use a repository's name to join its sessions."),
        )
        .child(Input::new(&input))
        .children(choices)
        .child(
            div()
                .pt(px(8.))
                .flex()
                .gap(px(8.))
                .when(linked, |row| {
                    row.child(ghost_button("project-unlink", "Unlink").on_click(cx.listener(
                        |desk, _, window, cx| desk.link_project(None, window, cx),
                    )))
                })
                .child(div().flex_1())
                .child(
                    Button::new("dialog-cancel")
                        .label("Cancel")
                        .ghost()
                        .on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))),
                )
                .child(
                    Button::new("project-ok")
                        .label("Link")
                        .primary()
                        .on_click(cx.listener(|desk, _, window, cx| {
                            let name = desk.input_value(InputSlot::Project, cx);
                            desk.link_project(Some(name), window, cx);
                        })),
                ),
        )
        .into_any_element()
}

/// The held-paste dialog's question.
pub(super) fn paste_question(title: &str, lines: usize) -> String {
    match lines {
        1 => format!("Paste a line break into {title}?"),
        lines => format!("Paste {lines} lines into {title}?"),
    }
}

pub(super) fn confirm(
    title: String,
    body: &str,
    label: &str,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
    run: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
) -> AnyElement {
    dialog_card(&title, colors, look)
        .border_color(from_hex(&colors.theme.danger).opacity(0.55))
        .child(
            div()
                .text_sm()
                .text_color(from_hex(&colors.subtext))
                .child(body.to_owned()),
        )
        .child(dialog_buttons(
            Button::new("confirm-ok")
                .label(label.to_owned())
                .danger()
                .on_click(cx.listener(move |desk, _, window, cx| run(desk, window, cx))),
            cx,
        ))
        .into_any_element()
}
