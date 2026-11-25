//! The sidebar's machines: this window's header, and every other machine
//! folded under its own, each opening to a glance at its sessions. A dot in
//! the machine's colour leads each line.

use crate::ui::tokens::Voice;
use rune::{El, col, el, text};
use rune_kit::SidebarRow;

use super::super::{Paint, Tokens, pressable};
use super::dot_size;
use crate::desk::Desk;
use crate::host_color;
use crate::machines::{Glance, LOCAL};

/// A machine's line: open or folded, and how many sessions it holds once known.
pub(super) fn host_header(
    label: &str,
    open: bool,
    count: Option<String>,
    t: &Tokens,
    press: impl Fn(&mut Desk) + 'static,
) -> El {
    let s = &t.space;
    let dim = t.roles.text.dim.paint();
    pressable(format!("host-{label}"), t)
        .w_full()
        .h(s.row_h)
        .px(s.s1)
        .gap(s.icon_gap)
        .radius(t.radius.sm)
        .items_center()
        .focusable(true)
        .role("button")
        .expanded(open)
        .label(label.to_owned())
        .on_press(press)
        .child(super::disclosure_mark(open, t))
        .child(host_color::dot(label, s.s2, t.roles.layer.sidebar.paint()))
        .child(
            text(label.to_owned())
                .font(t.ty.label.semibold())
                .color(t.roles.text.text.paint())
                .truncate(),
        )
        .child(el().grow(1.0))
        .children(count.map(|count| text(count).font(t.ty.caption).color(dim)))
}

/// A session on another machine (kit `SidebarRow`). Pressing it moves the
/// window there, unless that machine's only route is ssh.
fn glance_row(host: Option<String>, label: &str, glance: &Glance, dial: bool, t: &Tokens) -> El {
    let terminals = match glance.terminals {
        1 => "1 terminal".to_owned(),
        n => format!("{n} terminals"),
    };
    let mark = el().w(t.icon.grid).center().child(host_color::dot(
        label,
        dot_size(t),
        t.roles.layer.sidebar.paint(),
    ));
    let row = SidebarRow::new(glance.name.clone())
        .avatar(mark)
        .title_style(t.ty.label)
        .subtitle_color(t.roles.text.dim.paint())
        .detail(terminals)
        .height(t.space.row_h)
        .depth(1);
    if !dial {
        // Reached only by ssh: nothing to press, so no Tab stop either.
        return row
            .build(&t.kit)
            .key(format!("glance-{label}-{}", glance.name))
            .focusable(false);
    }
    let session = glance.name.clone();
    row.on_select(move |d: &mut Desk, _| d.open_on(host.clone(), session.clone()))
        .build(&t.kit)
        .key(format!("glance-{label}-{}", glance.name))
        .cursor(rune::Cursor::Pointer)
}

/// Every machine but this window's, folded until pressed. Its own list,
/// out of the sessions' (which selects as focus moves): reaching a glance
/// by arrow must never move the window to another machine.
pub(super) fn other_machines(desk: &Desk, t: &Tokens) -> El {
    let machines = &desk.machines;
    let local = machines.current.is_some().then_some((None, LOCAL, true));
    let remotes = machines
        .hosts
        .iter()
        .filter(|host| machines.current.as_ref() != Some(&host.name))
        .map(|host| {
            (
                Some(host.name.clone()),
                host.name.as_str(),
                !host.ssh_only && !host.paused,
            )
        });
    let s = &t.space;
    let mut list = col()
        .key("sidebar-machines")
        .role("list")
        .label("Other machines")
        .focus_group()
        .w_full()
        .gap(s.s0_5)
        .px(s.s2)
        .pt(s.s3);
    for (host, label, dial) in local.into_iter().chain(remotes) {
        let open = machines.expanded.contains(&host);
        let listed = machines.glances.get(&host);
        let count = match listed {
            Some(Ok(sessions)) => Some(sessions.len().to_string()),
            Some(Err(_)) => Some("offline".to_owned()),
            None => None,
        };
        let held = host.clone();
        list = list.child(host_header(label, open, count, t, move |d: &mut Desk| {
            d.toggle_host(held.clone());
        }));
        if !open {
            continue;
        }
        let note = |words: String| {
            el().w_full().px(s.s6).py(s.s0_5).child(
                text(words)
                    .font(t.ty.caption)
                    .color(t.roles.text.dim.paint())
                    .truncate(),
            )
        };
        list = match listed {
            Some(Ok(sessions)) if sessions.is_empty() => list.child(note("No sessions".to_owned())),
            Some(Ok(sessions)) => sessions.iter().fold(list, |list, glance| {
                list.child(glance_row(host.clone(), label, glance, dial, t))
            }),
            Some(Err(error)) => list.child(note(error.clone())),
            None => list.child(note("Listing…".to_owned())),
        };
    }
    list
}
