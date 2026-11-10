//! The last step: keys worth knowing, then a session.

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

pub(super) fn done(p: &Ink) -> El {
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
        .size(104.0, 104.0)
        .child(rune::icon(&icons::FOX, 96.0).color(p.text))
        .child(
            el().absolute(72.0, 72.0)
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
        .child(
            text("A few keys worth knowing, from your keymap.")
                .font(LEAD)
                .color(p.dim)
                .align(Align::Center),
        )
        .child(list)
        .child(el().h(4.0))
        .child(primary(
            "onboarding-start",
            "Start using phux",
            p,
            Desk::start_using,
        ))
}
