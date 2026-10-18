//! Notification panels and modal confirmation and text-entry dialogs.

use super::*;

/// Every notification this window has posted, newest first. Unread entries
/// carry a dot until the list closes; a click reveals the entry's terminal.
fn notification_rows(
    desk: &Desk,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
    agents: bool,
) -> Vec<AnyElement> {
    desk.feed
        .items()
        .filter(|entry| entry.kind.inbox() == agents)
        .map(|entry| {
            let id = entry.id;
            let decide = entry
                .approval
                .map(|approval| approval_buttons(desk, approval, id, cx));
            let edge = crate::app::view::notification_edge(entry, look);
            let age = crate::app::ui::ago(0, entry.at.elapsed().as_millis() as u64);
            div()
                .id(("notification", id as usize))
                .px(px(12.))
                .py(px(12.))
                .rounded(inset_radius(look))
                .border_1()
                .border_color(from_hex(&colors.theme.foreground).opacity(0.055))
                .bg(from_hex(&colors.theme.foreground).opacity(if entry.read {
                    0.015
                } else {
                    0.04
                }))
                .flex()
                .items_start()
                .gap(px(10.))
                .cursor_pointer()
                .hover(|style| style.bg(look.hover))
                .child(dot(7., edge).mt(px(5.)).when(entry.read, |dot| {
                    dot.bg(gpui::transparent_black())
                        .border_1()
                        .border_color(edge)
                }))
                .child(
                    crate::app::view::notification_text(entry, look, !entry.read, 2)
                        .children(decide),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(from_hex(&look.chrome.subtext))
                        .child(age),
                )
                .on_click(cx.listener(move |desk, _, window, cx| {
                    desk.open_notification(id, window, cx);
                }))
                .into_any_element()
        })
        .collect()
}

/// Approve and Deny for one held action. Both go quiet while its decision
/// is in flight; the server's answer takes the entry down or reports why not.
fn approval_buttons(
    desk: &Desk,
    approval: phux_protocol::ids::ApprovalId,
    entry: u64,
    cx: &mut Context<Desk>,
) -> AnyElement {
    use gpui_kit::base::Disableable;
    use phux_client_runtime::approvals::Decision;
    let busy = desk.approvals.deciding(approval);
    let button = |decision: Decision, label: &'static str, selector: &'static str| {
        div().debug_selector(move || selector.into()).child(
            Button::new((selector, entry as usize))
                .label(label)
                .small()
                .disabled(busy)
                .when(decision == Decision::Deny, |button| button.ghost())
                .on_click(cx.listener(move |desk, _, _, cx| {
                    cx.stop_propagation();
                    desk.feed.read(entry);
                    desk.decide_approval(approval, decision, cx);
                })),
        )
    };
    div()
        .pt(px(6.))
        .flex()
        .gap(px(6.))
        .child(button(Decision::Approve, "Approve", "approval-approve"))
        .child(button(Decision::Deny, "Deny", "approval-deny"))
        .into_any_element()
}

fn notification_heading(title: &str, unread: usize, look: &Look) -> AnyElement {
    muted(look, format!("{title} · {unread} unread alerts"))
        .px(px(10.))
        .pt(px(14.))
        .pb(px(8.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .into_any_element()
}

pub(super) fn notifications_panel(
    desk: &Desk,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let agent_rows = notification_rows(desk, colors, look, cx, true);
    let general_rows = notification_rows(desk, colors, look, cx, false);
    let unread_agents = desk
        .feed
        .unread()
        .filter(|entry| entry.kind.inbox())
        .count();
    let unread_general = desk.feed.unread().count() - unread_agents;
    let empty = desk.feed.is_empty();
    let mut rows = vec![notification_heading(
        "Inbox — agents and approvals",
        unread_agents,
        look,
    )];
    if agent_rows.is_empty() {
        rows.push(
            div()
                .px(px(12.))
                .py(px(16.))
                .rounded(inset_radius(look))
                .bg(from_hex(&colors.theme.foreground).opacity(0.025))
                .text_sm()
                .text_color(from_hex(&colors.muted))
                .child("No agent alerts or approvals.")
                .into_any_element(),
        );
    }
    rows.extend(agent_rows);
    if !general_rows.is_empty() {
        rows.push(notification_heading("Notifications", unread_general, look));
        rows.extend(general_rows);
    }
    sheet(look, 460.)
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .px(px(8.))
                .py(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(from_hex(&colors.theme.foreground))
                        .child("Notifications"),
                )
                .when(!empty, |header| {
                    header.child(ghost_button("notifications-clear", "Clear all").on_click(
                        cx.listener(|desk, _, window, cx| {
                            desk.execute("clear-notifications", window, cx)
                        }),
                    ))
                }),
        )
        .child(if empty {
            div()
                .px(px(20.))
                .py(px(28.))
                .border_1()
                .border_color(from_hex(&colors.theme.foreground).opacity(0.06))
                .bg(from_hex(&colors.theme.foreground).opacity(0.025))
                .text_sm()
                .text_color(from_hex(&colors.muted))
                .child("No notifications")
                .into_any_element()
        } else {
            div()
                .id("notification-list")
                .max_h(px(480.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(6.))
                .children(rows)
                .into_any_element()
        })
        .into_any_element()
}

fn dialog_card(title: &str, colors: &Palette, look: &Look) -> gpui::Div {
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

fn dialog_buttons(confirm: Button, cx: &mut Context<Desk>) -> impl IntoElement {
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
