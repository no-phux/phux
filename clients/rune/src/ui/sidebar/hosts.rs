//! The sidebar's machines: this window's header, and every other machine
//! folded under its own, each opening to a glance at its sessions. A dot in
//! the machine's colour leads each line.

use rune::{El, col, el, row, text};

use super::super::{CAPTION, Ink, LABEL_STRONG, NAME, icons, pressable};
use crate::desk::Desk;
use crate::host_color;
use crate::machines::{Glance, LOCAL};

/// A machine's line: open or folded, and how many sessions it holds once known.
pub(super) fn host_header(
    label: &str,
    open: bool,
    count: Option<String>,
    ink: &Ink,
    press: impl Fn(&mut Desk) + 'static,
) -> El {
    let chevron = if open {
        &icons::CHEVRON
    } else {
        &icons::CHEVRON_RIGHT
    };
    pressable(format!("host-{label}"), ink)
        .w_full()
        .h(26.0)
        .px(6.0)
        .gap(5.0)
        .radius(6.0)
        .items_center()
        .on_press(press)
        .child(rune::icon(chevron, 12.0).color(ink.dim))
        .child(host_color::dot(label, 8.0, ink.sidebar))
        .child(
            text(label.to_owned())
                .font(LABEL_STRONG)
                .color(ink.text)
                .truncate(),
        )
        .child(el().grow(1.0))
        .children(count.map(|count| text(count).font(CAPTION).color(ink.dim)))
}

/// A session on another machine. Pressing it moves the window there, unless
/// that machine's only route is ssh.
fn glance_row(host: Option<String>, label: &str, glance: &Glance, dial: bool, ink: &Ink) -> El {
    let terminals = match glance.terminals {
        1 => "1 terminal".to_owned(),
        n => format!("{n} terminals"),
    };
    let row = row()
        .key(format!("glance-{label}-{}", glance.name))
        .w_full()
        .min_h(26.0)
        .px(6.0)
        .py(4.0)
        .gap(6.0)
        .radius(7.0)
        .items_center()
        .child(
            el().w(14.0)
                .center()
                .child(host_color::dot(label, 5.0, ink.sidebar)),
        )
        .child(
            text(glance.name.clone())
                .font(NAME)
                .color(ink.text.alpha(0.62))
                .truncate(),
        )
        .child(el().grow(1.0))
        .child(text(terminals).font(CAPTION).color(ink.dim));
    if !dial {
        return row;
    }
    let session = glance.name.clone();
    row.hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(move |d: &mut Desk| d.open_on(host.clone(), session.clone()))
}

/// Every machine but this window's, folded until pressed.
pub(super) fn other_machines(desk: &Desk, ink: &Ink) -> El {
    let machines = &desk.machines;
    let local = machines.current.is_some().then_some((None, LOCAL, true));
    let remotes = machines
        .hosts
        .iter()
        .filter(|host| machines.current.as_ref() != Some(&host.name))
        .map(|host| (Some(host.name.clone()), host.name.as_str(), !host.ssh_only));
    let mut list = col().w_full().gap(1.0).px(8.0).pt(10.0);
    for (host, label, dial) in local.into_iter().chain(remotes) {
        let open = machines.expanded.contains(&host);
        let listed = machines.glances.get(&host);
        let count = match listed {
            Some(Ok(sessions)) => Some(sessions.len().to_string()),
            Some(Err(_)) => Some("offline".to_owned()),
            None => None,
        };
        let held = host.clone();
        list = list.child(host_header(label, open, count, ink, move |d: &mut Desk| {
            d.toggle_host(held.clone());
        }));
        if !open {
            continue;
        }
        let note = |words: String| {
            el().w_full()
                .px(26.0)
                .py(3.0)
                .child(text(words).font(CAPTION).color(ink.dim).truncate())
        };
        list = match listed {
            Some(Ok(sessions)) if sessions.is_empty() => list.child(note("No sessions".to_owned())),
            Some(Ok(sessions)) => sessions.iter().fold(list, |list, glance| {
                list.child(glance_row(host.clone(), label, glance, dial, ink))
            }),
            Some(Err(error)) => list.child(note(error.clone())),
            None => list.child(note("Listing…".to_owned())),
        };
    }
    list
}
