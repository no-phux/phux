//! The last step: an agent to start, keys worth knowing, then a session.

use super::*;

/// A chord as key caps: `⇧⌘S`, or `Ctrl+Shift+S`.
fn caps(chord: &str, p: &Ink) -> El {
    let parts: Vec<String> = if chord.contains('+') {
        chord.split('+').map(str::to_owned).collect()
    } else {
        chord.chars().map(String::from).collect()
    };
    row().gap(4.0).children(parts.into_iter().map(|part| {
        el().min_w(22.0)
            .h(22.0)
            .px(6.0)
            .radius(5.0)
            .center()
            .fill(p.current)
            .border(1.0, p.separator)
            .child(text(part).font(CAPTION).color(p.text))
    }))
}

/// Three keys worth knowing, from the keymap Rune runs.
const KEYS: [Shortcut; 3] = [
    Shortcut::Palette,
    Shortcut::SwitchSession,
    Shortcut::SplitRight,
];

/// On by default: without it a reboot, or often a logout, ends every session.
fn keep_alive(desk: &Desk, p: &Ink) -> El {
    col()
        .w_full()
        .max_w(400.0)
        .radius(12.0)
        .fill(p.raised.alpha(0.6))
        .border(1.0, p.separator)
        .child(crate::ui::settings::toggle(
            desk,
            "onboarding-keep-alive",
            "Keep sessions running after logout and reboot",
            "Runs phux as a systemd user service.",
            desk.onboarding.keep_alive,
            Desk::toggle_keep_alive,
            p,
        ))
}

/// Start an agent found here, so the first run ends with one at work; with
/// none, the short tour instead of a dead end.
fn start_agent(desk: &Desk, p: &Ink) -> El {
    let panel = col()
        .key("done-agents")
        .w_full()
        .max_w(400.0)
        .padding(14.0)
        .gap(10.0)
        .radius(12.0)
        .items_center()
        .fill(p.raised.alpha(0.6))
        .border(1.0, p.separator);
    let found = match &desk.onboarding.skills {
        Probe::Asking => return panel.child(note("Looking for agents on this computer…", p)),
        Probe::Ready(skills) => words::starters(skills),
        Probe::Unsupported | Probe::Failed(_) => Vec::new(),
    };
    if found.is_empty() {
        return panel
            .child(note(
                "No coding agent found on this computer. Install Claude Code, Codex or \
                 OpenCode and phux runs it here, with its status in the sidebar.",
                p,
            ))
            .child(quiet("done-tour", "Take the short tour", p).on_press(Desk::take_the_tour));
    }
    let buttons = found.into_iter().enumerate().map(|(at, id)| {
        let (fill, ink) = if at == 0 {
            (p.accent, p.on_accent)
        } else {
            (p.current, p.text)
        };
        let label = format!("Start {} here", words::agent_name(id));
        pill(format!("done-agent-{id}"), label, fill, ink, p)
            .on_press(move |d: &mut Desk| d.start_agent(id))
    });
    panel
        .child(note(
            "Start an agent in a pane with the phux skill set up. Its status shows in the sidebar.",
            p,
        ))
        .child(
            row()
                .gap(8.0)
                .flex_wrap()
                .justify_center()
                .children(buttons),
        )
}

pub(super) fn done(desk: &Desk, p: &Ink) -> El {
    let mut list = col()
        .w_full()
        .max_w(400.0)
        .radius(12.0)
        .fill(p.raised.alpha(0.6))
        .border(1.0, p.separator);
    for (at, command) in KEYS.into_iter().filter_map(keys::command).enumerate() {
        if at > 0 {
            list = list.child(
                el().w_full()
                    .px(14.0)
                    .child(el().w_full().h(1.0).fill(p.separator)),
            );
        }
        list = list.child(
            row()
                .key(format!("done-key-{at}"))
                .w_full()
                .h(44.0)
                .px(14.0)
                .items_center()
                .label(format!("{}, {}", command.title, command.hint()))
                .child(text(command.title).font(LABEL).color(p.text).a11y_hidden())
                .child(el().grow(1.0))
                .child(caps(&command.hint(), p).a11y_hidden()),
        );
    }
    let mark = el()
        .size(112.0, 112.0)
        .child(art(desk.fox_image(), 112.0))
        .child(
            el().absolute(80.0, 80.0)
                .size(28.0, 28.0)
                .radius(14.0)
                .center()
                .fill(p.marks.good)
                .child(rune::icon(&icons::CHECK, 16.0).color(crate::theme::on_fill(p.marks.good))),
        )
        .a11y_hidden();
    col()
        .w_full()
        .gap(12.0)
        .items_center()
        .child(mark)
        .child(
            text("You're all set")
                .font(TITLE)
                .color(p.text)
                .align(Align::Center)
                .role("heading")
                .level(1),
        )
        .child(start_agent(desk, p))
        .child(
            text("A few keys worth knowing, from your keymap.")
                .font(LEAD)
                .color(p.dim)
                .align(Align::Center),
        )
        .child(list)
        .children(words::KEEP_ALIVE.then(|| keep_alive(desk, p)))
        // Optional: the phone app, from the same screen Settings shows.
        .child(
            el().w_full()
                .max_w(400.0)
                .radius(12.0)
                .fill(p.raised.alpha(0.6))
                .border(1.0, p.separator)
                .child(super::super::pair::panel(desk, p, false)),
        )
        .child(el().h(4.0))
        .child(primary(
            "onboarding-start",
            "Start using phux",
            p,
            Desk::start_using,
        ))
}
