//! The machines sheet, and the sidebar control that opens it.

use rune::{Color, El, TextStyle, col, el, row, text, text_input};

use super::overlay::key_hint;
use super::settings::section;
use super::{CAPTION, Ink, LABEL, LABEL_STRONG, Sheet, icons, pressable};
use crate::desk::Desk;
use crate::host_color;
use crate::machines::{Host, LOCAL, Peer};
use phux_config_core::host_color::PALETTE;

pub fn switch(desk: &Desk, ink: &Ink) -> El {
    pressable("machine-switch", ink)
        .h(24.0)
        .px(6.0)
        .gap(5.0)
        .radius(6.0)
        .min_w(0.0)
        .items_center()
        .on_press(|d: &mut Desk| d.toggle_machines())
        .child(host_color::dot(desk.machines.label(), 8.0, ink.sidebar))
        .child(
            text(desk.machines.label().to_owned())
                .font(LABEL_STRONG)
                .color(ink.text)
                .truncate(),
        )
        .child(rune::icon(&icons::CHEVRON, 12.0).color(ink.dim))
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
    let color = if up {
        host_color::on(desk.machines.label(), ink.raised)
    } else {
        ink.dim
    };
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

/// The row's colour, pressed to show the swatches.
fn color_button(name: &str, ink: &Ink) -> El {
    let held = name.to_owned();
    pressable(format!("machine-color-{name}"), ink)
        .size(22.0, 22.0)
        .radius(11.0)
        .center()
        .label(format!("Colour for {name}"))
        .on_press(move |d: &mut Desk| d.pick_color(Some(held.clone())))
        .child(host_color::dot(name, 12.0, ink.raised))
}

/// Every palette swatch and Auto under the row whose colour is being picked; the
/// current pick wears a ring.
fn picker(name: &str, desk: &Desk, ink: &Ink) -> Option<El> {
    if desk.machines.picking.as_deref() != Some(name) {
        return None;
    }
    let (rgb, picked) = host_color::entry(name);
    let mut row = row().w_full().pl(40.0).pb(4.0).gap(6.0).items_center();
    for (i, (id, hex)) in PALETTE.iter().enumerate() {
        let held = name.to_owned();
        let on = picked && *hex == rgb;
        row = row.child(
            el().key(format!("machine-swatch-{name}-{id}"))
                .size(20.0, 20.0)
                .radius(10.0)
                .center()
                .border(2.0, if on { ink.text } else { Color::TRANSPARENT })
                .cursor(rune::Cursor::Pointer)
                .label((*id).to_owned())
                .on_press(move |d: &mut Desk| d.recolor_machine(held.clone(), id))
                .child(
                    el().size(14.0, 14.0)
                        .radius(7.0)
                        .fill(host_color::swatch(i, ink.raised)),
                ),
        );
    }
    let held = name.to_owned();
    let auto = pressable(format!("machine-swatch-{name}-auto"), ink)
        .px(7.0)
        .h(20.0)
        .radius(10.0)
        .items_center()
        .border(1.0, if picked { ink.separator } else { ink.text })
        .on_press(move |d: &mut Desk| d.recolor_machine(held.clone(), "auto"))
        .child(text("Auto").font(CAPTION).color(ink.text));
    Some(row.child(auto))
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
        .child(color_button(LOCAL, ink))
        .child(text(LOCAL).font(LABEL).color(ink.text).truncate())
        .child(el().grow(1.0))
        .children(
            current
                .then(|| current_state(desk, ink))
                .into_iter()
                .flatten(),
        )
}

fn machine(host: &Host, desk: &Desk, ink: &Ink) -> El {
    let name = host.name.clone();
    let current = desk.machines.current.as_ref() == Some(&host.name);
    let label = text(name.clone()).font(LABEL).color(ink.text).truncate();
    let held = name.clone();
    let (paused, pausing) = (host.paused, name.clone());
    let shell = row_shell(format!("machine-{name}"), ink);
    // A paused machine is not dialed, so its row opens nothing.
    let shell = if paused {
        shell
    } else {
        shell.on_press(move |d: &mut Desk| d.switch_machine(Some(held.clone())))
    };
    shell
        .child(color_button(&name, ink))
        .child(label)
        .child(el().grow(1.0))
        .children(
            host.ssh_only
                .then(|| text("over ssh").font(CAPTION).color(ink.dim)),
        )
        .children(paused.then(|| text("paused").font(CAPTION).color(ink.dim)))
        .child(chip(
            if paused { "Resume" } else { "Pause" },
            format!("machine-pause-{name}"),
            ink,
            move |d: &mut Desk| d.pause_machine(pausing.clone(), !paused),
        ))
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
        .child(local_machine(desk, ink))
        .children(picker(LOCAL, desk, ink));
    for host in &list.hosts {
        body = body
            .child(machine(host, desk, ink))
            .children(picker(&host.name, desk, ink));
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
    let foot = row().w_full().child(key_hint("esc", "close", ink));
    let sheet = Sheet {
        key: "machines",
        title: "Machines",
        width: 420.0,
        top: 96.0,
        scrim: 0.3,
        close: Desk::close_machines,
    };
    Some(sheet.build(None, body, foot, ink))
}
