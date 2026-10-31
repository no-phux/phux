//! The machines sheet, and the sidebar control that opens it.

use rune::{Color, El, TextStyle, Transition, col, el, row, text, text_input};

use super::settings::section;
use super::{CAPTION, Ink, LABEL, LABEL_STRONG, pressable};
use crate::desk::Desk;
use crate::machines::{Host, LOCAL, Peer};

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

/// A small word to press in a row.
fn chip(label: &'static str, key: String, ink: &Ink, press: impl Fn(&mut Desk) + 'static) -> El {
    pressable(key, ink)
        .px(6.0)
        .radius(5.0)
        .on_press(press)
        .child(text(label).font(CAPTION).color(ink.accent))
}

/// What the current machine's row says: how the connection is, and a way to
/// retry when it's down.
fn current_state(desk: &Desk, ink: &Ink) -> Vec<El> {
    let word = desk.link_word();
    let up = word == "connected";
    let color = if up { ink.accent } else { ink.dim };
    let mut state = vec![text(word).font(CAPTION).color(color)];
    if !up {
        state.push(chip(
            "Retry",
            "machine-retry".to_owned(),
            ink,
            Desk::reconnect,
        ));
    }
    state
}

/// A registered machine's removal, asked once.
fn removal(name: &str, desk: &Desk, ink: &Ink) -> Vec<El> {
    if desk.machines.removing.as_deref() != Some(name) {
        let held = name.to_owned();
        let ask = move |d: &mut Desk| d.ask_remove(Some(held.clone()));
        return vec![chip("Remove", format!("machine-remove-{name}"), ink, ask)];
    }
    let held = name.to_owned();
    vec![
        chip(
            "Remove for good",
            format!("machine-remove-yes-{name}"),
            ink,
            move |d: &mut Desk| d.remove_machine(held.clone()),
        ),
        chip(
            "Keep",
            format!("machine-remove-no-{name}"),
            ink,
            |d: &mut Desk| d.ask_remove(None),
        ),
    ]
}

fn row_shell(key: String, ink: &Ink) -> El {
    pressable(key, ink)
        .w_full()
        .px(10.0)
        .py(7.0)
        .gap(8.0)
        .radius(7.0)
        .items_center()
}

fn local_machine(desk: &Desk, ink: &Ink) -> El {
    let current = desk.machines.current.is_none();
    row_shell(format!("machine-{LOCAL}"), ink)
        .on_press(|d: &mut Desk| d.switch_machine(None))
        .child(text(LOCAL).font(LABEL).color(ink.text).truncate())
        .child(el().grow(1.0))
        .children(
            current
                .then(|| current_state(desk, ink))
                .into_iter()
                .flatten(),
        )
}

/// A machine a window can't dial is listed, with what it needs, but doesn't
/// press: its only route is ssh.
fn machine(host: &Host, desk: &Desk, ink: &Ink) -> El {
    let name = host.name.clone();
    let current = desk.machines.current.as_ref() == Some(&host.name);
    let label = text(name.clone()).font(LABEL).color(ink.text).truncate();
    let row = if host.needs_direct {
        row().w_full().px(10.0).py(7.0).gap(8.0).items_center()
    } else {
        let held = name.clone();
        row_shell(format!("machine-{name}"), ink)
            .on_press(move |d: &mut Desk| d.switch_machine(Some(held.clone())))
    };
    row.child(label)
        .child(el().grow(1.0))
        .children(
            host.needs_direct
                .then(|| text("needs a direct route").font(CAPTION).color(ink.dim)),
        )
        .children(
            current
                .then(|| current_state(desk, ink))
                .into_iter()
                .flatten(),
        )
        .children(removal(&name, desk, ink))
}

fn peer(peer: &Peer, ink: &Ink) -> El {
    let held = peer.clone();
    let shell = if peer.online {
        row_shell(format!("peer-{}", peer.name), ink)
            .on_press(move |d: &mut Desk| d.add_peer(&held))
    } else {
        row().w_full().px(10.0).py(7.0).gap(8.0).items_center()
    };
    shell
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
        .child(local_machine(desk, ink));
    for host in &list.hosts {
        body = body.child(machine(host, desk, ink));
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
    match &list.outcome {
        Some(Ok(done)) => {
            body = body.child(text(done.clone()).font(CAPTION).color(ink.accent).wrap());
        }
        Some(Err(why)) => {
            body = body.child(text(why.clone()).font(CAPTION).color(ink.danger).wrap());
        }
        None => {}
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
