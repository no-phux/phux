//! The machines sheet, and the sidebar control that opens it.

use rune::{Color, El, TextStyle, Transition, col, el, row, text, text_input};

use super::settings::section;
use super::{CAPTION, Ink, LABEL, LABEL_STRONG, pressable};
use crate::desk::Desk;
use crate::machines::{LOCAL, Peer};

pub fn switch(desk: &Desk, ink: &Ink) -> El {
    row()
        .key("machine-switch")
        .w_full()
        .px(14.0)
        .pb(6.0)
        .gap(4.0)
        .items_center()
        .cursor(rune::Cursor::Pointer)
        .on_press(|d: &mut Desk| d.toggle_machines())
        .child(
            text(desk.machines.label().to_owned())
                .font(CAPTION)
                .color(ink.dim)
                .truncate(),
        )
        .child(text("▾").font(CAPTION).color(ink.dim))
}

fn machine(name: Option<String>, current: bool, ink: &Ink) -> El {
    let label = name.clone().unwrap_or_else(|| LOCAL.to_owned());
    pressable(format!("machine-{label}"), ink)
        .w_full()
        .px(10.0)
        .py(7.0)
        .gap(8.0)
        .radius(7.0)
        .items_center()
        .on_press(move |d: &mut Desk| d.switch_machine(name.clone()))
        .child(text(label).font(LABEL).color(ink.text).truncate())
        .child(el().grow(1.0))
        .children(current.then(|| text("connected").font(CAPTION).color(ink.accent)))
}

fn peer(peer: &Peer, ink: &Ink) -> El {
    let held = peer.clone();
    pressable(format!("peer-{}", peer.name), ink)
        .w_full()
        .px(10.0)
        .py(7.0)
        .gap(8.0)
        .radius(7.0)
        .items_center()
        .on_press(move |d: &mut Desk| d.add_peer(&held))
        .child(
            text(peer.name.clone())
                .font(LABEL)
                .color(ink.text)
                .truncate(),
        )
        .child(el().grow(1.0))
        .child(
            text(if peer.online { "Set up" } else { "offline" })
                .font(CAPTION)
                .color(if peer.online { ink.accent } else { ink.dim }),
        )
}

fn field(desk: &Desk, ink: &Ink) -> El {
    row()
        .w_full()
        .gap(8.0)
        .items_center()
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .px(10.0)
                .py(7.0)
                .radius(7.0)
                .fill(ink.separator)
                .child(
                    text_input(&desk.machines.field, TextStyle::new(13.0, 400.0).lh(18.0))
                        .key("machine-field")
                        .single_line()
                        .grow(1.0)
                        .color(ink.text)
                        .placeholder("me@host", ink.dim)
                        .on_edit(|d: &mut Desk| &mut d.machines.field)
                        .on_input_key(|d: &mut Desk, event, tx| d.machines_key(event, tx))
                        .on_submit(|d: &mut Desk, _| d.add_typed_machine()),
                ),
        )
        .child(
            el().key("machine-add")
                .px(12.0)
                .py(7.0)
                .radius(6.0)
                .fill(ink.accent)
                .cursor(rune::Cursor::Pointer)
                .on_press(|d: &mut Desk| d.add_typed_machine())
                .child(text("Set up").font(CAPTION).color(ink.on_accent)),
        )
}

pub fn sheet(desk: &Desk, ink: &Ink) -> Option<El> {
    let list = &desk.machines;
    if !list.open {
        return None;
    }
    let mut body = col()
        .w_full()
        .padding(16.0)
        .gap(6.0)
        .child(text("Machines").font(LABEL_STRONG).color(ink.text))
        .child(machine(None, list.current.is_none(), ink));
    for host in &list.hosts {
        let current = list.current.as_ref() == Some(host);
        body = body.child(machine(Some(host.clone()), current, ink));
    }
    if !list.peers.is_empty() {
        body = body
            .child(el().h(6.0))
            .child(section("On your tailnet", ink));
        for tailnet in &list.peers {
            body = body.child(self::peer(tailnet, ink));
        }
    }
    body = body
        .child(el().h(6.0))
        .child(section("Add a machine", ink))
        .child(field(desk, ink))
        .child(
            text("Uses your ssh access, and installs phux there if it's missing.")
                .font(CAPTION)
                .color(ink.dim)
                .wrap(),
        );
    if let Some(step) = &list.adding {
        body = body.child(text(step.clone()).font(CAPTION).color(ink.accent).wrap());
    }
    if let Some(error) = &list.error {
        body = body.child(text(error.clone()).font(CAPTION).color(ink.danger).wrap());
    }
    let panel = col()
        .key("machines")
        .w(420.0)
        .max_h(560.0)
        .radius(12.0)
        .clip_rounded(12.0)
        .fill(ink.raised)
        .border(1.0, ink.separator)
        .shadow(18.0, 56.0, Color::BLACK.alpha(0.5))
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(body);
    Some(
        el().key("machines-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(Color::BLACK.alpha(0.3))
            .transition(Transition::opacity())
            .modal(true, |d: &mut Desk| d.close_machines())
            .child(col().w_full().pt(96.0).items_center().child(panel)),
    )
}
