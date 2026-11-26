//! The last step: an agent to start, keys worth knowing, then a session.

use super::*;
use crate::ui::tokens::Voice;

/// A chord as kit key caps (`Keybind`): `⇧⌘S`, or `Ctrl+Shift+S`.
fn caps(chord: &str, t: &Tokens) -> El {
    let parts: Vec<String> = if chord.contains('+') {
        chord.split('+').map(str::to_owned).collect()
    } else {
        chord.chars().map(String::from).collect()
    };
    rune_kit::Keybind::new(&parts).build(&t.kit)
}

/// Three keys worth knowing, from the keymap Rune runs.
const KEYS: [Shortcut; 3] = [
    Shortcut::Palette,
    Shortcut::SwitchSession,
    Shortcut::SplitRight,
];

/// A single-column card: opaque raised, `radius.md`.
fn card(t: &Tokens) -> El {
    col()
        .w_full()
        .max_w(GEOMETRY.list_max)
        .radius(t.radius.md)
        .fill(t.roles.layer.raised.paint())
        .border(t.space.hairline, t.roles.border.base.paint())
}

/// On by default: without it a reboot, or often a logout, ends every session.
fn keep_alive(desk: &Desk, t: &Tokens) -> El {
    let toggle = crate::ui::settings::Toggle {
        key: "onboarding-keep-alive",
        title: "Keep sessions running after logout and reboot",
        detail: "Runs phux as a systemd user service.",
        on: desk.onboarding.keep_alive,
        press: Desk::toggle_keep_alive,
        enabled: true,
    };
    card(t).child(crate::ui::settings::toggle(desk, toggle, t))
}

/// Start an agent found here, so the first run ends with one at work; with
/// none, the short tour instead of a dead end.
fn start_agent(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let panel = card(t)
        .key("done-agents")
        .padding(s.s4)
        .gap(s.s3)
        .items_center();
    let found = match &desk.onboarding.skills {
        Probe::Asking => return panel.child(note("Looking for agents on this computer…", t)),
        Probe::Ready(skills) => words::starters(skills),
        Probe::Unsupported | Probe::Failed(_) => Vec::new(),
    };
    if found.is_empty() {
        return panel
            .child(note(
                "No coding agent found on this computer. Install Claude Code, Codex or \
                 OpenCode and phux runs it here, with its status in the sidebar.",
                t,
            ))
            .child(quiet("done-tour", "Take the short tour", t).on_press(Desk::take_the_tour));
    }
    // The first agent is the step's suggestion; the others are choices.
    let buttons = found.into_iter().enumerate().map(|(at, id)| {
        let label = format!("Start {} here", words::agent_name(id));
        action(format!("done-agent-{id}"), label, at == 0, t)
            .on_press(move |d: &mut Desk| d.start_agent(id))
    });
    panel
        .child(note(
            "Start an agent in a pane with the phux skill set up. Its status shows in the sidebar.",
            t,
        ))
        .child(
            row()
                .gap(s.s2)
                .flex_wrap()
                .justify_center()
                .children(buttons),
        )
}

/// The brand fox with the done mark on its shoulder.
fn mark(desk: &Desk, t: &Tokens) -> El {
    let done = t.status.tone(phux_design::Tone::Done).ink;
    let side = t.icon.button.md;
    // The mark sits a step in from the fox's corner.
    let at = GEOMETRY.fox_done - side - t.space.s1;
    el().size(GEOMETRY.fox_done, GEOMETRY.fox_done)
        .child(art(desk.fox_image(), GEOMETRY.fox_done))
        .child(
            el().absolute(at, at)
                .size(side, side)
                .radius(t.radius.full)
                .center()
                .fill(done)
                .child(rune::icon(&icons::CHECK, t.icon.grid).color(crate::theme::on_fill(done))),
        )
        .a11y_hidden()
}

pub(super) fn done(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let rule = t.roles.border.base.paint();
    let mut list = card(t);
    for (at, command) in KEYS.into_iter().filter_map(keys::command).enumerate() {
        if at > 0 {
            list = list.child(
                el().w_full()
                    .px(s.s3)
                    .child(el().w_full().h(s.hairline).fill(rule)),
            );
        }
        list = list.child(
            row()
                .key(format!("done-key-{at}"))
                .w_full()
                .min_h(s.touch_min)
                .px(s.s3)
                .items_center()
                .label(format!("{}, {}", command.title, command.hint()))
                .child(
                    text(command.title)
                        .font(t.ty.label.medium())
                        .color(t.roles.text.text.paint())
                        .a11y_hidden(),
                )
                .child(el().grow(1.0))
                .child(caps(&command.hint(), t).a11y_hidden()),
        );
    }
    col()
        .w_full()
        .gap(s.s3)
        .items_center()
        .child(mark(desk, t))
        .child(
            text("You're all set")
                .font(t.ty.display)
                .color(t.roles.text.text.paint())
                .align(Align::Center)
                .role("heading")
                .level(1),
        )
        .child(start_agent(desk, t))
        .child(
            text("Shortcuts you will use most.")
                .font(t.ty.lead)
                .color(t.roles.text.dim.paint())
                .align(Align::Center),
        )
        .child(list)
        .children(words::KEEP_ALIVE.then(|| keep_alive(desk, t)))
        // Optional: the phone app, from the same screen Settings shows.
        .child(card(t).child(crate::ui::pair::panel(desk, t, false)))
        .child(el().h(s.s1))
        .child(primary(
            "onboarding-start",
            "Start using phux",
            t,
            Desk::start_using,
        ))
}
