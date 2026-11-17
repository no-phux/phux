//! The Bring your setup step.

use super::*;

/// An agent (or Skip) as a big square to press: a neutral tile on the
/// control outline, the icon in `text` (D8: no decorative accent).
fn agent_tile(
    key: String,
    glyph: &'static rune::Icon,
    word: String,
    t: &Tokens,
    press: impl Fn(&mut Desk) + 'static,
) -> El {
    let s = &t.space;
    col()
        .gap(s.icon_gap)
        .items_center()
        .child(
            crate::ui::pressable(key, t)
                .size(GEOMETRY.agent_tile_w, GEOMETRY.agent_tile_h)
                .radius(t.radius.lg)
                .continuous_corners()
                .center()
                .fill(t.roles.layer.raised.paint())
                .border(s.hairline, t.roles.border.control.paint())
                .focusable(true)
                .role("button")
                .label(word.clone())
                .on_press(press)
                .child(
                    rune::icon(glyph, t.icon.size.xl)
                        .color(t.roles.text.text.paint())
                        .a11y_hidden(),
                ),
        )
        .child(
            text(word)
                .font(t.ty.caption)
                .color(t.roles.text.text.paint())
                .a11y_hidden(),
        )
}

pub(super) fn migrate(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let ob = &desk.onboarding;
    let head = heading(
        &icons::BRING,
        "Bring your setup",
        "Choose an agent to find your key bindings and bring them to phux.",
        t,
    );
    let body = match &ob.migrate {
        Migrate::Starting(agent) => panel(t)
            .max_w(GEOMETRY.status_max)
            .items_center()
            .child(note(format!("Starting {}…", words::agent_name(agent)), t)),
        Migrate::Running(agent, _) => panel(t)
            .key("migrate-status")
            .max_w(GEOMETRY.status_max)
            .items_center()
            .child(tone_badge(t, State::Working, None))
            .child(
                text(format!(
                    "{} is bringing over your config.",
                    words::agent_name(agent)
                ))
                .font(semibold(t.ty.label))
                .color(t.roles.text.text.paint())
                .align(Align::Center)
                .wrap(),
            )
            .child(note("It's working in the pane on the right.", t))
            .child(quiet("migrate-cancel", "Cancel", t).on_press(Desk::cancel_migration)),
        Migrate::Choosing | Migrate::Failed(_) => {
            let tiles = ob.migrators().into_iter().map(|agent| {
                agent_tile(
                    format!("migrate-{agent}"),
                    &icons::PROMPT,
                    words::agent_name(agent),
                    t,
                    move |d: &mut Desk| d.migrate_with(agent),
                )
            });
            let failed = match &ob.migrate {
                Migrate::Failed(why) => Some(
                    text(why.clone())
                        .font(t.ty.caption)
                        .color(t.status.tone(phux_design::Tone::Failed).ink)
                        .wrap(),
                ),
                _ => None,
            };
            panel(t)
                .max_w(GEOMETRY.machines_max)
                .items_center()
                .child(
                    row()
                        .gap(s.s4)
                        .flex_wrap()
                        .justify_center()
                        .children(tiles)
                        .child(
                            el().w(s.hairline)
                                .h(GEOMETRY.agent_tile_h)
                                .fill(t.roles.border.base.paint()),
                        )
                        .child(agent_tile(
                            "migrate-skip".into(),
                            &icons::SKIP,
                            "Skip".into(),
                            t,
                            Desk::onboarding_continue,
                        )),
                )
                .child(note(
                    "The agent brings over key bindings from tmux, zellij, Ghostty and herdr, \
                     and asks before replacing one. Themes and fonts don't move.",
                    t,
                ))
                .children(failed)
                .child(
                    quiet(
                        "migrate-copy",
                        if ob.copied { "Copied" } else { "Copy prompt" },
                        t,
                    )
                    .on_press(Desk::copy_migration_prompt),
                )
        }
    };
    col()
        .w_full()
        .items_center()
        .gap(s.s5)
        .child(head)
        .child(body)
}
