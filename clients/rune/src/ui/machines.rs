//! The machines sheet, and the sidebar control that opens it.
//!
//! Machine rows are OWN rows on tokens (§4.1.3: kit `List` is label-only);
//! their actions are kit buttons; the colour swatches are a radio group.

use rune::{Color, El, TextStyle, col, el, row, text, text_input};
use rune_kit::{Button, ButtonVariant, ControlSize};

use super::settings::{grown, section, semibold};
use super::tokens::{MONO, Paint, Tokens};
use super::{Sheet, icons, pressable};
use crate::desk::Desk;
use crate::desk::machines::ssh::Offered;
use crate::host_color;
use crate::machines::{Host, LOCAL, Peer};
use phux_config_core::host_color::PALETTE;

mod checklist;
#[cfg(test)]
mod tests;

/// The sheet's geometry: component sizes, not style.
struct Geometry {
    sheet_w: f32,
    /// Where the sheet sits when the window has room.
    sheet_top: f32,
    /// What a row's swatches and checklist indent by: past its colour dot.
    nested: f32,
}

const GEOMETRY: Geometry = Geometry {
    sheet_w: 420.0,
    sheet_top: 96.0,
    nested: 40.0,
};

/// A machine noun (a host name) in the chrome's mono face (§1.4 voice).
fn mono(style: TextStyle) -> TextStyle {
    TextStyle {
        family: MONO,
        ..style
    }
}

pub fn switch(desk: &Desk, p: &Tokens) -> El {
    let t = p;
    let s = &t.space;
    pressable("machine-switch", t)
        .min_h(s.pointer_min)
        .px(s.s1)
        .gap(s.icon_gap)
        .radius(t.radius.sm)
        .min_w(0.0)
        .items_center()
        .focusable(true)
        .label(format!("Machine: {}", desk.machines.label()))
        .on_press(|d: &mut Desk| d.toggle_machines())
        .child(host_color::dot(
            desk.machines.label(),
            s.s2,
            t.roles.layer.sidebar.paint(),
        ))
        .child(
            text(desk.machines.label().to_owned())
                .font(mono(semibold(t.ty.label)))
                .color(t.roles.text.text.paint())
                .truncate()
                .a11y_hidden(),
        )
        .child(
            rune::icon(&icons::CHEVRON, t.icon.size.sm)
                .color(t.roles.text.dim.paint())
                .a11y_hidden(),
        )
}

/// A small action in a row: a kit ghost button, in the Tab order.
fn chip(label: &'static str, key: String, t: &Tokens, press: impl Fn(&mut Desk) + 'static) -> El {
    Button::new(label)
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press(press)
        .build(&t.kit)
        .key(key)
        .focusable(true)
        .min_h(grown(t.ty.caption, t.space.icon_gap))
}

/// A destructive action: the kit danger button (§1.10).
fn danger(label: &'static str, key: String, t: &Tokens, press: impl Fn(&mut Desk) + 'static) -> El {
    Button::new(label)
        .variant(ButtonVariant::Danger)
        .control_size(ControlSize::Small)
        .on_press(press)
        .build(&t.kit)
        .key(key)
        .focusable(true)
        .min_h(grown(t.ty.caption, t.space.icon_gap))
}

/// Opens or closes the machine's What works here.
fn works(machine: Option<String>, t: &Tokens) -> El {
    let key = format!("machine-works-{}", machine.as_deref().unwrap_or(LOCAL));
    chip("What works", key, t, move |d: &mut Desk| {
        d.toggle_checklist(machine.clone());
    })
}

/// What the current machine's row says: how the connection is, and a way
/// to retry when it's down. Down is the `failed` tone with the word `lost`
/// when the link is gone (§1.3).
fn current_state(desk: &Desk, t: &Tokens) -> Vec<El> {
    let word = desk.link_word();
    let up = word == "connected";
    if up {
        let color = host_color::on(desk.machines.label(), t.roles.layer.raised.paint());
        return vec![text(word).font(t.ty.caption).color(color)];
    }
    vec![
        super::badge::tone_badge(t, phux_design::State::Lost, Some(word.to_owned())),
        chip("Retry", "machine-retry".to_owned(), t, Desk::reconnect),
    ]
}

/// A registered machine's removal, asked once.
fn removal(name: &str, desk: &Desk, t: &Tokens) -> Vec<El> {
    if desk.machines.removing.as_deref() != Some(name) {
        let held = name.to_owned();
        let ask = move |d: &mut Desk| d.ask_remove(Some(held.clone()));
        return vec![chip("Remove", format!("machine-remove-{name}"), t, ask)];
    }
    let held = name.to_owned();
    vec![
        danger(
            "Remove for good",
            format!("machine-remove-yes-{name}"),
            t,
            move |d: &mut Desk| d.remove_machine(held.clone()),
        ),
        chip(
            "Keep",
            format!("machine-remove-no-{name}"),
            t,
            |d: &mut Desk| d.ask_remove(None),
        ),
    ]
}

/// The row's colour, pressed to show the swatches.
fn color_button(name: &str, t: &Tokens) -> El {
    let held = name.to_owned();
    let side = t.space.pointer_min;
    pressable(format!("machine-color-{name}"), t)
        .size(side, side)
        .radius(t.radius.full)
        .center()
        .focusable(true)
        .label(format!("Colour for {name}"))
        .on_press(move |d: &mut Desk| d.pick_color(Some(held.clone())))
        .child(host_color::dot(
            name,
            t.space.s3,
            t.roles.layer.raised.paint(),
        ))
}

/// Every palette swatch and Auto under the row whose colour is being
/// picked: a radio group, each swatch a focusable radio named by its
/// colour; the current pick wears the focus-weight ring in `text`.
fn picker(name: &str, desk: &Desk, t: &Tokens) -> Option<El> {
    if desk.machines.picking.as_deref() != Some(name) {
        return None;
    }
    let s = &t.space;
    let r = &t.roles;
    let (rgb, picked) = host_color::entry(name);
    let mut group = row()
        .key(format!("machine-swatches-{name}"))
        .w_full()
        .pl(GEOMETRY.nested)
        .pb(s.s1)
        .gap(s.icon_gap)
        .items_center()
        .flex_wrap()
        .role("radiogroup")
        .label(format!("Colour for {name}"));
    let side = s.pointer_min;
    for (i, (id, hex)) in PALETTE.iter().enumerate() {
        let held = name.to_owned();
        let on = picked && *hex == rgb;
        group = group.child(
            pressable(format!("machine-swatch-{name}-{id}"), t)
                .size(side, side)
                .radius(t.radius.full)
                .center()
                .border(
                    s.focus_ring.width,
                    if on {
                        r.text.text.paint()
                    } else {
                        Color::TRANSPARENT
                    },
                )
                .focusable(true)
                .role("radio")
                .checked(on)
                .label((*id).to_owned())
                .on_press(move |d: &mut Desk| d.recolor_machine(held.clone(), id))
                .child(
                    el().size(t.icon.size.md, t.icon.size.md)
                        .radius(t.radius.full)
                        .fill(host_color::swatch(i, r.layer.raised.paint())),
                ),
        );
    }
    let held = name.to_owned();
    let auto = pressable(format!("machine-swatch-{name}-auto"), t)
        .px(s.s2)
        .min_h(side)
        .radius(t.radius.full)
        .items_center()
        .border(
            s.hairline,
            if picked {
                r.border.control.paint()
            } else {
                r.text.text.paint()
            },
        )
        .focusable(true)
        .role("radio")
        .checked(!picked)
        .label("Auto")
        .on_press(move |d: &mut Desk| d.recolor_machine(held.clone(), "auto"))
        .child(
            text("Auto")
                .font(t.ty.caption)
                .color(r.text.text.paint())
                .a11y_hidden(),
        );
    Some(group.child(auto))
}

/// An OWN row (§4.1.3): `row_h` tall at least, `radius.sm`, the hover and
/// press washes, focusable; `current` is the window's own machine, drawn
/// with §1.10's selected treatment.
fn row_shell(key: String, current: bool, t: &Tokens) -> El {
    let s = &t.space;
    let shell = pressable(key, t)
        .w_full()
        .min_h(s.row_h)
        .px(s.row_inset_x)
        .py(s.s1)
        .gap(s.s2)
        .radius(t.radius.sm)
        .items_center()
        .focusable(true)
        .focus_selected(current);
    if current {
        super::lifted(shell, t)
    } else {
        shell
    }
}

fn name(words: String, t: &Tokens) -> El {
    text(words)
        .font(mono(t.ty.label))
        .color(t.roles.text.text.paint())
        .truncate()
        .min_w(0.0)
        .shrink()
}

fn local_machine(desk: &Desk, t: &Tokens) -> El {
    let current = desk.machines.current.is_none();
    row_shell(format!("machine-{LOCAL}"), current, t)
        .label(LOCAL)
        .on_press(|d: &mut Desk| d.switch_machine(None))
        .child(color_button(LOCAL, t))
        .child(name(LOCAL.to_owned(), t))
        .child(el().grow(1.0))
        .child(works(None, t))
        .children(
            current
                .then(|| current_state(desk, t))
                .into_iter()
                .flatten(),
        )
}

fn machine(host: &Host, desk: &Desk, t: &Tokens) -> El {
    let host_name = host.name.clone();
    let current = desk.machines.current.as_ref() == Some(&host.name);
    let held = host_name.clone();
    let (paused, pausing) = (host.paused, host_name.clone());
    let detail = |words: &'static str| {
        text(words)
            .font(t.ty.caption)
            .color(t.roles.text.dim.paint())
    };
    let shell = row_shell(format!("machine-{host_name}"), current, t).label(host_name.clone());
    // A paused machine is not dialed, so its row opens nothing.
    let shell = if paused {
        shell
    } else {
        shell.on_press(move |d: &mut Desk| d.switch_machine(Some(held.clone())))
    };
    shell
        .child(color_button(&host_name, t))
        .child(name(host_name.clone(), t))
        .child(el().grow(1.0))
        .children(host.ssh_only.then(|| detail("over ssh")))
        .children(paused.then(|| detail("paused")))
        .children((current || !paused && !host.ssh_only).then(|| works(Some(host_name.clone()), t)))
        .child(chip(
            if paused { "Resume" } else { "Pause" },
            format!("machine-pause-{host_name}"),
            t,
            move |d: &mut Desk| d.pause_machine(pausing.clone(), !paused),
        ))
        .children(
            current
                .then(|| current_state(desk, t))
                .into_iter()
                .flatten(),
        )
        .children(removal(&host_name, desk, t))
}

fn peer(peer: &Peer, t: &Tokens) -> El {
    let s = &t.space;
    let held = peer.clone();
    let key = format!("peer-{}", peer.name);
    let shell = if peer.online {
        row_shell(key, false, t)
            .label(peer.name.clone())
            .on_press(move |d: &mut Desk| d.add_peer(&held))
    } else {
        // Offline, it has nothing to press: the disabled row (§1.10).
        row()
            .key(key)
            .w_full()
            .min_h(s.row_h)
            .px(s.row_inset_x)
            .py(s.s1)
            .gap(s.s2)
            .items_center()
            .disabled(true)
    };
    let (word, ink) = if peer.online {
        ("Set up", t.roles.accent.accent_text)
    } else {
        ("offline", t.roles.text.faint)
    };
    shell
        .child(name(peer.name.clone(), t))
        .child(el().grow(1.0))
        .child(text(word).font(t.ty.caption).color(ink.paint()))
}

fn ssh_host(alias: &str, desk: &Desk, t: &Tokens) -> El {
    let held = alias.to_owned();
    let r = &t.roles;
    let word = match desk.machines.offer_state(alias) {
        Offered::Waiting => text("Waiting").font(t.ty.caption).color(r.text.dim.paint()),
        Offered::Running(_) => super::badge::tone_badge(
            t,
            phux_design::State::Working,
            Some("Setting up…".to_owned()),
        ),
        Offered::Failed(_) => {
            super::badge::tone_badge(t, phux_design::State::Failed, Some("Try again".to_owned()))
        }
        _ => text("Set up")
            .font(t.ty.caption)
            .color(r.accent.accent_text.paint()),
    };
    row_shell(format!("ssh-host-{alias}"), false, t)
        .label(alias.to_owned())
        .on_press(move |d: &mut Desk| d.add_ssh_host(&held))
        .child(name(alias.to_owned(), t))
        .child(el().grow(1.0))
        .child(word)
}

/// The address field: Rune's native editor (native input targets it by
/// key) in a kit `TextInput`'s frame, and the step's one primary action.
fn field(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    row()
        .w_full()
        .gap(s.s2)
        .items_center()
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .min_h(s.pointer_min + s.s1)
                .px(s.s2)
                .py(s.s1)
                .radius(t.radius.sm)
                .items_center()
                .fill(r.layer.ground.paint())
                .border(s.hairline, r.border.control.paint())
                .child(
                    text_input(&desk.machines.field, mono(t.ty.body))
                        .key("machine-field")
                        .label("Computer SSH address")
                        .single_line()
                        .grow(1.0)
                        .color(r.text.text.paint())
                        .placeholder("me@host", r.text.faint.paint())
                        .on_edit(|d: &mut Desk| &mut d.machines.field)
                        .on_input_key(|d: &mut Desk, event, tx| d.machines_key(event, tx))
                        .on_submit(|d: &mut Desk, _| d.add_typed_machine()),
                ),
        )
        .child(
            Button::new("Set up")
                .variant(ButtonVariant::Submit)
                .control_size(ControlSize::Large)
                .on_press(|d: &mut Desk| d.add_typed_machine())
                .build(&t.kit)
                .key("machine-add")
                .focusable(true)
                .min_h(grown(t.ty.body, t.space.s2)),
        )
}

/// A pairing link waiting to be accepted: where it dials and the name it
/// registers, and whether that replaces a machine already set up.
fn offer(desk: &Desk, t: &Tokens) -> Option<El> {
    let s = &t.space;
    let offer = desk.machines.offer.as_ref()?;
    let target = &offer.request.target;
    let mut ask = col().w_full().gap(s.s1).pb(s.s2).child(
        text(format!("Pair with {} as {target}?", offer.peer))
            .font(semibold(t.ty.label))
            .color(t.roles.text.text.paint())
            .wrap(),
    );
    if desk.machines.hosts.iter().any(|host| &host.name == target) {
        ask = ask.child(
            text(format!(
                "This replaces the machine already named {target} and its credentials."
            ))
            .font(t.ty.caption)
            .color(t.status.tone(phux_design::Tone::Failed).ink)
            .wrap(),
        );
    }
    let buttons = row()
        .gap(s.s2)
        .child(
            Button::new("Pair")
                .variant(ButtonVariant::Submit)
                .control_size(ControlSize::Large)
                .on_press(Desk::accept_pairing)
                .build(&t.kit)
                .key("machine-pair-yes")
                .focusable(true)
                .min_h(grown(t.ty.body, t.space.s2)),
        )
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Large)
                .on_press(Desk::decline_pairing)
                .build(&t.kit)
                .key("machine-pair-no")
                .focusable(true)
                .min_h(grown(t.ty.body, t.space.s2)),
        );
    Some(ask.child(buttons))
}

/// One line of progress or outcome under the field, in its tone.
fn outcome(words: String, tone: phux_design::Tone, t: &Tokens) -> El {
    text(words)
        .font(t.ty.caption)
        .color(t.status.tone(tone).ink)
        .wrap()
}

pub fn sheet(desk: &Desk, p: &Tokens) -> Option<El> {
    let t = p;
    let s = &t.space;
    let list = &desk.machines;
    if !list.open {
        return None;
    }
    let mut body = col()
        .w_full()
        .padding(s.s4)
        .gap(s.s1)
        .children(offer(desk, t))
        .child(local_machine(desk, t))
        .children(picker(LOCAL, desk, t))
        .children(checklist::checklist(None, desk, t));
    for host in &list.hosts {
        body = body
            .child(machine(host, desk, t))
            .children(picker(&host.name, desk, t))
            .children(checklist::checklist(Some(&host.name), desk, t));
    }
    let head = |title| col().w_full().pt(s.s3).pb(s.s1).child(section(title, t));
    if !list.peers.is_empty() {
        body = body.child(head("On your tailnet"));
        for tailnet in &list.peers {
            body = body.child(self::peer(tailnet, t));
        }
    }
    let config: Vec<_> = list.unregistered_ssh().collect();
    if !config.is_empty() {
        body = body.child(head("In your ssh config"));
        for alias in config {
            body = body.child(ssh_host(alias, desk, t));
        }
    }
    body = body
        .child(head("Add a machine"))
        .child(field(desk, t))
        .child(
            text("Uses your ssh access, and installs phux there if it's missing.")
                .font(t.ty.caption)
                .color(t.roles.text.dim.paint())
                .wrap(),
        );
    if let Some(step) = &list.adding {
        body = body.child(outcome(step.clone(), phux_design::Tone::Working, t));
    }
    if let Some(error) = &list.error {
        body = body.child(outcome(error.clone(), phux_design::Tone::Failed, t));
    }
    match &list.outcome {
        Some(Ok(done)) => body = body.child(outcome(done.clone(), phux_design::Tone::Done, t)),
        Some(Err(why)) => body = body.child(outcome(why.clone(), phux_design::Tone::Failed, t)),
        None => {}
    }
    let foot = row()
        .w_full()
        .gap(s.icon_gap)
        .items_center()
        .child(super::keycap("esc", t))
        .child(
            text("close")
                .font(t.ty.caption)
                .color(t.roles.text.dim.paint()),
        );
    let sheet = Sheet {
        key: "machines",
        title: "Machines",
        width: GEOMETRY.sheet_w,
        top: GEOMETRY.sheet_top,
        close: Desk::close_machines,
    };
    Some(sheet.build(None, body, foot, t))
}
