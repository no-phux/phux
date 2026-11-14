//! The Add your other machines step: hosts from the ssh config and the
//! tailnet, ticked and set up one after another.

use super::*;
use crate::desk::machines::ssh::Offered;

fn checkbox(on: bool, p: &Ink) -> El {
    let frame = el()
        .size(16.0, 16.0)
        .radius(4.0)
        .center()
        .border(1.0, if on { p.accent } else { p.dim });
    if on {
        frame
            .fill(p.accent)
            .child(rune::icon(&icons::CHECK, 12.0).color(p.on_accent))
    } else {
        frame
    }
}

fn status(words: impl Into<String>, ink: Color) -> El {
    text(words.into()).font(CAPTION).color(ink).truncate()
}

fn machine(target: String, name: String, state: Offered, p: &Ink) -> El {
    let line = row().w_full().h(30.0).px(10.0).gap(10.0).items_center();
    let label = text(name.clone())
        .font(LABEL_STRONG)
        .color(p.text)
        .truncate();
    let row = match &state {
        Offered::Pick(on) => pressable(format!("onboarding-machine-{target}"), p)
            .w_full()
            .h(30.0)
            .px(10.0)
            .gap(10.0)
            .radius(7.0)
            .items_center()
            .focusable(true)
            .role("checkbox")
            .label(name)
            .checked(*on)
            .on_press(move |d: &mut Desk| d.pick_machine(&target))
            .child(checkbox(*on, p))
            .child(label.a11y_hidden()),
        Offered::Done => line
            .child(rune::icon(&icons::CHECK, 14.0).color(p.marks.good))
            .child(label)
            .child(el().grow(1.0))
            .child(status("Set up", p.marks.good)),
        Offered::Waiting => line
            .child(checkbox(true, p))
            .child(label)
            .child(el().grow(1.0))
            .child(status("Waiting", p.dim)),
        Offered::Running(step) => line
            .child(checkbox(true, p))
            .child(label)
            .child(el().grow(1.0))
            .child(status(step.clone(), p.accent)),
        Offered::Failed(_) => line
            .child(checkbox(false, p))
            .child(label)
            .child(el().grow(1.0))
            .child(status("Couldn't set up", p.danger)),
    };
    let why = match state {
        Offered::Failed(why) => Some(
            el().w_full()
                .px(36.0)
                .child(text(why).font(COPY).color(p.danger).wrap()),
        ),
        _ => None,
    };
    col().w_full().child(row).children(why)
}

pub(super) fn machines(desk: &Desk, p: &Ink) -> El {
    let list = &desk.machines;
    let head = heading(
        &icons::MACHINES,
        "Add your other machines",
        "Tick the ones you use. phux installs itself on each over ssh and starts \
         there, and their sessions show in the sidebar.",
        p,
    );
    let offered = list.offered();
    let mut panel = panel(p).max_w(460.0).gap(4.0);
    if offered.is_empty() {
        panel = panel.child(note(
            "No machines in your ssh config or on your tailnet. Add one any time from \
             Settings → Machines.",
            p,
        ));
    }
    let mut picked = 0;
    for (target, name) in offered {
        let state = list.offer_state(&target);
        picked += usize::from(state == Offered::Pick(true));
        panel = panel.child(machine(target, name, state, p));
    }
    let set_up = (picked > 0).then(|| {
        let label = match picked {
            1 => "Set up 1 machine".to_owned(),
            n => format!("Set up {n} machines"),
        };
        pill(
            "onboarding-machines-set-up",
            label,
            p.accent,
            p.on_accent,
            p,
        )
        .on_press(Desk::set_up_picked)
        .self_center()
    });
    col()
        .w_full()
        .items_center()
        .gap(18.0)
        .child(head)
        .child(panel.children(set_up))
        .child(note(
            "Uses your ssh access. Machines keep setting up if you continue.",
            p,
        ))
}
