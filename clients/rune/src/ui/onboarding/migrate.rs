//! The Bring your setup step.

use super::*;

/// An agent (or Skip) as a big square to press.
fn agent_tile(
    key: String,
    glyph: &'static rune::Icon,
    word: String,
    strong: bool,
    p: &Ink,
    press: impl Fn(&mut Desk) + 'static,
) -> El {
    col()
        .gap(6.0)
        .items_center()
        .child(
            pressable(key, p)
                .size(84.0, 68.0)
                .radius(14.0)
                .continuous_corners()
                .center()
                .fill(if strong { p.accent } else { p.current })
                .border(1.0, p.separator)
                .focusable(true)
                .role("button")
                .label(word.clone())
                .on_press(press)
                .child(
                    rune::icon(glyph, 26.0)
                        .color(if strong { p.on_accent } else { p.dim })
                        .a11y_hidden(),
                ),
        )
        .child(text(word).font(CAPTION).color(p.text).a11y_hidden())
}

pub(super) fn migrate(desk: &Desk, p: &Ink) -> El {
    let ob = &desk.onboarding;
    let head = heading(
        &icons::BRING,
        "Bring your setup",
        "Choose an agent to find your key bindings and bring them to phux.",
        p,
    );
    let body = match &ob.migrate {
        Migrate::Starting(agent) => panel(p)
            .max_w(420.0)
            .items_center()
            .child(note(format!("Starting {}…", words::agent_name(agent)), p)),
        Migrate::Running(agent, _) => panel(p)
            .key("migrate-status")
            .max_w(420.0)
            .items_center()
            .child(
                text(format!(
                    "{} is bringing over your config.",
                    words::agent_name(agent)
                ))
                .font(LABEL_STRONG)
                .color(p.text)
                .align(Align::Center)
                .wrap(),
            )
            .child(note("It's working in the pane on the right.", p))
            .child(quiet("migrate-cancel", "Cancel", p).on_press(Desk::cancel_migration)),
        Migrate::Choosing | Migrate::Failed(_) => {
            let tiles = ob.migrators().into_iter().map(|agent| {
                agent_tile(
                    format!("migrate-{agent}"),
                    &icons::PROMPT,
                    words::agent_name(agent),
                    true,
                    p,
                    move |d: &mut Desk| d.migrate_with(agent),
                )
            });
            let failed = match &ob.migrate {
                Migrate::Failed(why) => Some(text(why.clone()).font(COPY).color(p.danger).wrap()),
                _ => None,
            };
            panel(p)
                .max_w(460.0)
                .items_center()
                .child(
                    row()
                        .gap(14.0)
                        .flex_wrap()
                        .justify_center()
                        .children(tiles)
                        .child(el().w(1.0).h(68.0).fill(p.separator))
                        .child(agent_tile(
                            "migrate-skip".into(),
                            &icons::SKIP,
                            "Skip".into(),
                            false,
                            p,
                            Desk::onboarding_continue,
                        )),
                )
                .child(note(
                    "The agent brings over key bindings from tmux, zellij, Ghostty and herdr, \
                     and asks before replacing one. Themes and fonts don't move.",
                    p,
                ))
                .children(failed)
                .child(
                    quiet(
                        "migrate-copy",
                        if ob.copied { "Copied" } else { "Copy prompt" },
                        p,
                    )
                    .on_press(Desk::copy_migration_prompt),
                )
        }
    };
    col()
        .w_full()
        .items_center()
        .gap(18.0)
        .child(head)
        .child(body)
}
