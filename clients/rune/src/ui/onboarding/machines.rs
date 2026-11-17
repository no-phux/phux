//! The Add your other machines step: hosts from the ssh config and the
//! tailnet, ticked (kit `Checkbox`) and set up one after another.

use super::*;
use crate::desk::machines::ssh::Offered;
use crate::ui::tokens::Voice;

/// A host name in the chrome's mono face (§1.4 voice).
fn host(name: String, t: &Tokens) -> El {
    text(name)
        .font(t.ty.label.semibold().machine())
        .color(t.roles.text.text.paint())
}

fn machine(target: String, name: String, state: Offered, t: &Tokens) -> El {
    let s = &t.space;
    let line = || {
        row()
            .w_full()
            .min_h(s.row_h + s.s0_5)
            .px(s.s3)
            .gap(s.s3)
            .items_center()
    };
    let row =
        match &state {
            // A failed one ticks again to retry.
            Offered::Pick(_) | Offered::Failed(_) => {
                let on = state == Offered::Pick(true);
                let failed = matches!(state, Offered::Failed(_));
                let key = format!("onboarding-machine-{target}");
                // The kit checkbox is the whole row: its name is the host's.
                let tick = rune_kit::Checkbox::new(on)
                    .label(name.clone())
                    .hide_label()
                    .on_toggle(move |d: &mut Desk| d.pick_machine(&target))
                    .build(t.controls(false));
                line().child(
                    t.kit
                        .interactive(tick.key(key))
                        .grow(1.0)
                        .radius(t.radius.sm)
                        .cursor(rune::Cursor::Pointer)
                        .child(host(name, t).a11y_hidden())
                        .child(el().grow(1.0))
                        .children(failed.then(|| {
                            tone_badge(t, State::Failed, Some("Couldn't set up".to_owned()))
                        })),
                )
            }
            Offered::Done => line()
                .child(tone_badge(t, State::Done, Some("Set up".to_owned())))
                .child(host(name, t)),
            Offered::Waiting => line().child(host(name, t)).child(el().grow(1.0)).child(
                text("Waiting")
                    .font(t.ty.caption)
                    .color(t.roles.text.dim.paint()),
            ),
            Offered::Running(step) => line()
                .child(host(name, t))
                .child(el().grow(1.0))
                .child(tone_badge(t, State::Working, Some(step.clone()))),
        };
    let why = match state {
        Offered::Failed(why) => Some(
            col().w_full().min_w(0.0).px(s.s3).child(
                text(why)
                    .font(t.ty.caption)
                    .color(t.status.tone(phux_design::Tone::Failed).ink)
                    .wrap(),
            ),
        ),
        _ => None,
    };
    col().w_full().child(row).children(why)
}

pub(super) fn machines(desk: &Desk, t: &Tokens) -> El {
    let list = &desk.machines;
    let head = heading(
        &icons::MACHINES,
        "Add your other machines",
        "Tick the ones you use. phux installs itself on each over ssh and starts \
         there, and their sessions show in the sidebar.",
        t,
    );
    let offered = list.offered();
    let mut panel = panel(t).max_w(GEOMETRY.machines_max).gap(t.space.s1);
    if offered.is_empty() {
        panel = panel.child(note(
            "No machines in your ssh config or on your tailnet. Add one any time from \
             Settings → Machines.",
            t,
        ));
    }
    let mut picked = 0;
    for (target, name) in offered {
        let state = list.offer_state(&target);
        picked += usize::from(state == Offered::Pick(true));
        panel = panel.child(machine(target, name, state, t));
    }
    let set_up = (picked > 0).then(|| {
        let label = match picked {
            1 => "Set up 1 machine".to_owned(),
            n => format!("Set up {n} machines"),
        };
        action("onboarding-machines-set-up", label, true, t)
            .on_press(Desk::set_up_picked)
            .self_center()
    });
    col()
        .w_full()
        .items_center()
        .gap(t.space.s5)
        .child(head)
        .child(panel.children(set_up))
        .child(note(
            "Uses your ssh access. Machines keep setting up if you continue.",
            t,
        ))
}
