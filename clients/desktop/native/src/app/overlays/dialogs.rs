//! Small dialogs: rename, session, add machine, and destructive confirmations.

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, Window, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::{Disableable, Sizable};

use super::{InputSlot, sheet};
use crate::app::color::from_hex;
use crate::app::desk::{Desk, TailnetView};
use crate::app::host;
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
    let setup = &desk.add_machine;
    let adding = setup.adding.is_some();
    let muted = from_hex(&colors.muted);
    dialog_card("Add Machine", colors, look)
        .child(div().text_xs().text_color(muted).child(
            "Set up a machine you can reach over ssh. phux starts its server, pairs with it, and connects to it directly after that.",
        ))
        .child(tailnet_section(desk, colors, cx))
        .child(Input::new(&target))
        .child(Input::new(&name))
        .child(
            Checkbox::new("host-install")
                .label("Install or update phux on the machine if needed")
                .checked(setup.install)
                .disabled(adding)
                .on_click(cx.listener(|desk, checked: &bool, _, cx| desk.set_install(*checked, cx))),
        )
        .children(setup.progress.clone().map(|step| {
            div()
                .debug_selector(|| "host-add-progress".to_owned())
                .text_xs()
                .text_color(from_hex(&colors.subtext))
                .child(format!(
                    "Setting up {}: {step}",
                    setup.adding.as_deref().unwrap_or_default()
                ))
        }))
        .children(setup.error.clone().map(|error| {
            div()
                .debug_selector(|| "host-add-error".to_owned())
                .text_xs()
                .text_color(from_hex(&colors.theme.danger))
                .child(error)
        }))
        .child(dialog_buttons(
            Button::new("host-add-ok")
                .label(if adding { "Setting Up…" } else { "Add Machine" })
                .primary()
                .loading(adding)
                .on_click(cx.listener(|desk, _, window, cx| {
                    let target = desk.input_value(InputSlot::HostTarget, cx);
                    let name = desk.input_value(InputSlot::HostName, cx);
                    desk.apply_add_host(target, name, window, cx);
                })),
            cx,
        ))
        .into_any_element()
}

/// The tailnet's machines to pick from, or the one step that gets there:
/// get Tailscale, open it, connect it, or sign in.
fn tailnet_section(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    let muted = from_hex(&colors.muted);
    let note = |text: String| div().text_xs().text_color(muted).child(text);
    let tailnet = match &desk.add_machine.tailnet {
        TailnetView::Unknown | TailnetView::Loading => {
            return note("Looking for machines on your tailnet…".into()).into_any_element();
        }
        TailnetView::Failed(error) => return note(error.clone()).into_any_element(),
        TailnetView::Ready(tailnet) => tailnet,
    };
    if tailnet.running() {
        return peer_list(tailnet, colors, cx);
    }
    let signing_in = desk.add_machine.signing_in;
    let (text, action) = tailnet_step(&tailnet.state, tailnet.auth_url.is_some());
    let row = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(note(text.to_owned()).flex_1());
    let Some(action) = action else {
        return row.into_any_element();
    };
    let button = Button::new("tailnet-action")
        .label(if signing_in { "Waiting…" } else { action })
        .small()
        .loading(signing_in);
    let button = match tailnet.state.as_str() {
        "not_installed" => {
            button.on_click(|_, _, cx| cx.open_url("https://tailscale.com/download"))
        }
        _ => match tailnet.auth_url.clone() {
            Some(url) => button.on_click(move |_, _, cx| cx.open_url(&url)),
            None => button.on_click(cx.listener(|desk, _, _, cx| desk.refresh_tailnet(true, cx))),
        },
    };
    row.child(
        div()
            .debug_selector(|| "tailnet-action".to_owned())
            .child(button),
    )
    .into_any_element()
}

/// What a client that is not connected says, and the button that moves it on.
pub(super) fn tailnet_step(state: &str, has_link: bool) -> (&'static str, Option<&'static str>) {
    match state {
        "needs_login" if has_link => (
            "Finish signing in to Tailscale in your browser; your machines appear here after.",
            Some("Open Sign-In Page"),
        ),
        "needs_login" => (
            "Sign in to Tailscale to pick a machine from your tailnet.",
            Some("Sign In to Tailscale"),
        ),
        "stopped" => ("Tailscale is disconnected.", Some("Connect Tailscale")),
        "not_running" => (
            "Tailscale is installed but not running.",
            Some("Start Tailscale"),
        ),
        "not_installed" => (
            "With Tailscale, your machines are listed here to pick from.",
            Some("Get Tailscale"),
        ),
        "needs_machine_auth" => (
            "Waiting for a tailnet admin to approve this computer.",
            None,
        ),
        _ => ("Tailscale is connecting…", None),
    }
}

/// The tailnet's Mac and Linux machines; choosing one fills the fields.
fn peer_list(tailnet: &host::Tailnet, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    let muted = from_hex(&colors.muted);
    if tailnet.peers.is_empty() {
        return div()
            .text_xs()
            .text_color(muted)
            .child("No other Mac or Linux machines are on your tailnet.")
            .into_any_element();
    }
    let rows = tailnet.peers.iter().enumerate().map(|(index, peer)| {
        let status = format!(
            "{} · {}",
            peer.os,
            if peer.online { "online" } else { "offline" }
        );
        let action = match &peer.registered {
            Some(name) if *name == peer.name => div()
                .text_xs()
                .text_color(muted)
                .child("Added")
                .into_any_element(),
            Some(name) => div()
                .text_xs()
                .text_color(muted)
                .child(format!("Added as {name}"))
                .into_any_element(),
            None => {
                let chosen = peer.clone();
                ghost_button(("tailnet-peer", index), "Choose")
                    .on_click(cx.listener(move |desk, _, window, cx| {
                        desk.choose_peer(&chosen, window, cx);
                    }))
                    .into_any_element()
            }
        };
        div()
            .debug_selector({
                let name = peer.name.clone();
                move || format!("tailnet-peer-{name}")
            })
            .flex()
            .items_center()
            .gap(px(8.))
            .py(px(2.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .gap(px(8.))
                    .text_sm()
                    .text_color(if peer.online {
                        from_hex(&colors.theme.foreground)
                    } else {
                        muted
                    })
                    .child(peer.name.clone())
                    .child(div().text_xs().text_color(muted).child(status)),
            )
            .child(action)
    });
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(tailnet.tailnet.as_ref().map_or_else(
                    || "On your tailnet".to_owned(),
                    |name| format!("On your tailnet ({name})"),
                )),
        )
        .child(
            div()
                .id("tailnet-peers")
                .max_h(px(168.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows),
        )
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
