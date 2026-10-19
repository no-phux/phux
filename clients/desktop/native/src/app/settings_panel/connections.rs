//! Saved machines, their available actions, and this window's connection.

use gpui::prelude::*;
use gpui::{Context, WeakEntity, Window, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{IconName, Sizable};
use phux_client_runtime::control::Status;

use super::super::desk::{Desk, Modal};
use super::super::fleet::Reach;
use super::super::ui::ghost_button;
use super::{click_runs, fact};

/// A machine row's button, findable by its element id.
fn machine_button(
    id: String,
    label: &'static str,
    handle: &WeakEntity<Desk>,
    run: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
) -> impl IntoElement {
    let selector = id.clone();
    div()
        .debug_selector(move || selector.clone())
        .child(ghost_button(id, label).on_click(click_runs(handle, run)))
}

/// The one action a machine's state allows: this window's own route
/// reconnects, another unavailable machine retries, a connected one opens.
pub(in super::super) fn machine_action(reach: &Reach, current: bool) -> Option<&'static str> {
    match (reach, current) {
        (Reach::Unavailable(_), true) => Some("Reconnect"),
        (Reach::Unavailable(_), false) => Some("Retry"),
        (Reach::Connected, false) => Some("Open"),
        (_, _) => None,
    }
}

/// One machine with its live state and the actions that state allows. Saved
/// machines stay listed while disconnected or empty.
fn machine_item(
    desk: &Desk,
    handle: &WeakEntity<Desk>,
    host: Option<&str>,
    route: &str,
) -> SettingItem {
    let reach = desk.machine_reach(host);
    let current = host == desk.remote.as_deref();
    let mut detail = format!("{route} · {}", reach.label());
    if current {
        detail.push_str(" · this window");
    }
    if let Reach::Unavailable(error) = &reach {
        detail.push_str(&format!(" · {error}"));
    }
    let id = host.unwrap_or("local").to_owned();
    let target = host.map(str::to_owned);
    let action = machine_action(&reach, current);
    // Another machine's inventory link can be dropped; this window's own
    // route only reconnects.
    let disconnect = !current && !matches!(reach, Reach::Unavailable(_));
    let handle = handle.clone();
    SettingItem::new(
        host.map_or_else(
            || "This computer".to_owned(),
            super::super::machine_name::display,
        ),
        SettingField::render(move |_, _, _| {
            let (open, drop) = (target.clone(), target.clone());
            div()
                .flex()
                .gap(px(8.))
                .when_some(action, |row, label| {
                    row.child(machine_button(
                        format!("settings-machine-{label}-{id}"),
                        label,
                        &handle,
                        move |desk, window, cx| match label {
                            "Reconnect" => desk.reconnect(cx),
                            "Retry" => desk.retry_fleet(cx),
                            _ => {
                                desk.close_modal(window, cx);
                                desk.open_machine(open.clone(), cx);
                                desk.after_command(window, cx);
                            }
                        },
                    ))
                })
                .when(disconnect, |row| {
                    row.child(machine_button(
                        format!("settings-machine-Disconnect-{id}"),
                        "Disconnect",
                        &handle,
                        move |desk, _, cx| desk.disconnect_machine(drop.clone(), cx),
                    ))
                })
                .when_some(target.clone(), |row, name| {
                    row.child(machine_button(
                        format!("settings-remove-host-{name}"),
                        "Forget…",
                        &handle,
                        move |desk, window, cx| {
                            let name = name.clone();
                            desk.open_modal(Modal::RemoveHost { name }, window, cx);
                        },
                    ))
                })
        }),
    )
    .description(detail)
}

fn hosts_group(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingGroup {
    let buttons = handle.clone();
    let controls = SettingItem::new(
        "Machines",
        SettingField::render(move |_, _, _| {
            div()
                .flex()
                .gap(px(8.))
                .child(
                    ghost_button("settings-add-host", "Add machine…")
                        .on_click(click_runs(&buttons, |desk, window, cx| {
                            desk.open_modal(Modal::AddHost, window, cx)
                        })),
                )
                .child(
                    ghost_button("settings-refresh-hosts", "Refresh all")
                        .on_click(click_runs(&buttons, |desk, _, cx| desk.retry_fleet(cx))),
                )
        }),
    )
    .description("Sessions from every registered machine appear together in Work.");
    let local = machine_item(desk, handle, None, "Local server on this computer");
    let hosts = desk
        .hosts
        .iter()
        .map(|host| machine_item(desk, handle, Some(&host.name), &host.endpoint));
    let empty = desk.hosts.is_empty().then(|| {
        fact(
            "Remote machines",
            "None added yet. Add machine… enrolls one over ssh.".into(),
        )
    });
    SettingGroup::new()
        .title("Hosts")
        .item(controls)
        .item(local)
        .items(hosts)
        .items(empty)
}

pub(super) fn connection_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let (status, _) = desk.connection_label();
    let reconnect = handle.clone();
    let attached = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.status == Status::Attached);
    let action = SettingItem::new(
        "Reconnect",
        SettingField::render(move |_, _, _| {
            Button::new("conn-reconnect")
                .label("Reconnect")
                .small()
                .map(|button| if attached { button } else { button.primary() })
                .on_click(click_runs(&reconnect, |desk, _, cx| desk.reconnect(cx)))
        }),
    )
    .description("Reconnect and restore this window.");
    SettingPage::new("Hosts & connections")
        .icon(IconName::Network)
        .group(hosts_group(desk, handle))
        .group(
            SettingGroup::new()
                .item(fact("Status", status))
                .item(action),
        )
}
