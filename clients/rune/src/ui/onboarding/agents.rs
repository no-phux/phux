//! The Agents step (and permissions, on macOS) and its grant sheet.

use super::*;
use crate::ui::tokens::Voice;

/// One of the Agents step's cards: opaque raised, `radius.lg`.
fn card(key: &'static str, glyph: &'static rune::Icon, title: &'static str, t: &Tokens) -> El {
    let s = &t.space;
    col()
        .key(key)
        .w(GEOMETRY.card_w)
        .min_h(GEOMETRY.card_min_h)
        .padding(s.s5)
        .gap(s.s3)
        .items_center()
        .radius(t.radius.lg)
        .continuous_corners()
        .fill(t.roles.layer.raised.paint())
        .border(s.hairline, t.roles.border.base.paint())
        .child(tile(glyph, GEOMETRY.card_tile, t))
        .child(
            text(title)
                .font(t.ty.lead.semibold())
                .color(t.roles.text.text.paint())
                .align(Align::Center)
                .role("heading")
                .level(2),
        )
}

fn skill_card(desk: &Desk, t: &Tokens) -> El {
    let ob = &desk.onboarding;
    let card = card("onboarding-skill", &icons::PUZZLE, "phux agent skill", t)
        .child(badge("Recommended", t))
        .child(note(
            "Teaches your agents to drive phux: sessions, panes and asks.",
            t,
        ))
        .child(el().grow(1.0));
    let skills = match &ob.skills {
        Probe::Asking => return card.child(note("Checking…", t)),
        Probe::Unsupported => return card.child(note(UPDATE, t)),
        Probe::Failed(why) => {
            return card
                .child(note(why.clone(), t))
                .child(quiet("onboarding-install", "Try again", t).on_press(Desk::install_skill));
        }
        Probe::Ready(skills) => skills,
    };
    if ob.installing {
        return card.child(note("Installing…", t));
    }
    // With no agent installed yet, the shared skill setup writes still counts.
    let mut wanted: Vec<_> = skills.iter().filter(|s| s.detected).collect();
    if wanted.is_empty() {
        wanted = skills.iter().filter(|s| s.installed()).collect();
    }
    let all = !wanted.is_empty() && wanted.iter().all(|s| s.installed());
    let rows = wanted.iter().filter(|_| !all).map(|skill| {
        let state = if skill.installed() {
            text(skill.label())
                .font(t.ty.caption)
                .color(t.status.tone(phux_design::Tone::Done).ink)
        } else {
            text(skill.label())
                .font(t.ty.caption)
                .color(t.roles.text.dim.paint())
        };
        row()
            .w_full()
            .gap(t.space.icon_gap)
            .child(
                text(words::agent_name(&skill.agent))
                    .font(t.ty.label.semibold())
                    .color(t.roles.text.text.paint()),
            )
            .child(el().grow(1.0))
            .child(
                state
                    .truncate()
                    .help(skill.hook_detail().unwrap_or_default()),
            )
    });
    let card = card.children(rows);
    if all {
        card.child(good("Installed", t))
    } else {
        card.child(
            action("onboarding-install", "Install".into(), true, t).on_press(Desk::install_skill),
        )
    }
}

/// Opt-in: it adds a line to the shell rc, so it is never the accent.
fn shim_card(desk: &Desk, t: &Tokens) -> El {
    let card = card("onboarding-shims", &icons::WINDOW, "Open agents in phux", t)
        .child(note(
            "Plain claude, codex, opencode and pi start inside phux. Adds one line to your shell config.",
            t,
        ))
        .child(el().grow(1.0));
    match &desk.onboarding.shims {
        None => card.child(quiet("onboarding-shim", "Set up", t).on_press(Desk::install_shims)),
        Some(Probe::Asking) => card.child(note("Setting up…", t)),
        Some(Probe::Unsupported) => card.child(note(UPDATE, t)),
        Some(Probe::Failed(why)) => card
            .child(note(why.clone(), t))
            .child(quiet("onboarding-shim", "Try again", t).on_press(Desk::install_shims)),
        Some(Probe::Ready(agents)) if agents.is_empty() => {
            card.child(note("No agents found on this computer.", t))
        }
        Some(Probe::Ready(agents)) => card
            .child(note(
                format!("{}. Open a new terminal to use them.", agents.join(", ")),
                t,
            ))
            .child(good("Set up", t)),
    }
}

fn permissions_card(desk: &Desk, t: &Tokens) -> El {
    let card = card(
        "onboarding-permissions",
        &icons::HAND,
        "System permissions",
        t,
    );
    let card = match &desk.onboarding.permissions {
        Probe::Asking => card.child(tone_badge(t, State::Working, Some("Checking…".to_owned()))),
        Probe::Ready(perms) => {
            let granted = perms.granted();
            let all = Service::ALL.len();
            let state = if granted == all {
                State::Done
            } else {
                State::NeedsYou
            };
            card.child(tone_badge(
                t,
                state,
                Some(format!("{granted} of {all} granted")),
            ))
        }
        Probe::Unsupported | Probe::Failed(_) => card,
    };
    let card = card
        .child(note("Some agent work needs macOS to say yes first.", t))
        .child(el().grow(1.0));
    match &desk.onboarding.permissions {
        Probe::Unsupported => card.child(note(UPDATE, t)),
        Probe::Failed(why) => card.child(note(why.clone(), t)),
        Probe::Ready(perms) if perms.granted() == Service::ALL.len() => {
            card.child(good("All granted", t))
        }
        _ => card.child(quiet("onboarding-grant", "Grant…", t).on_press(Desk::open_grant_sheet)),
    }
}

pub(super) fn agents(desk: &Desk, t: &Tokens) -> El {
    col()
        .w_full()
        .items_center()
        .gap(t.space.s5)
        .child(heading(
            &icons::PUZZLE,
            words::PROMISE.agents_title,
            words::PROMISE.agents_lead,
            t,
        ))
        .child(
            row()
                .gap(t.space.s4)
                .flex_wrap()
                .justify_center()
                .child(skill_card(desk, t))
                .child(shim_card(desk, t))
                .children(PERMISSIONS.then(|| permissions_card(desk, t))),
        )
}

/// The sheet over the Agents step: each permission, why, and its pane.
/// It is the OWN sheet's look (`layer.raised`, `radius.lg`, the overlay
/// tier, the one scrim, the entry) in its own composition: the window
/// docks System Settings beside this panel, so the panel reports its
/// layout, which `Sheet::build` has no hook for.
pub(super) fn grant_sheet(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let perms = desk.onboarding.permissions.ready();
    let mut rows = col()
        .w_full()
        .radius(t.radius.md)
        .fill(r.layer.ground.paint())
        .border(s.hairline, r.border.base.paint());
    for (at, service) in Service::ALL.into_iter().enumerate() {
        if at > 0 {
            rows = rows.child(el().w_full().h(s.hairline).fill(r.border.base.paint()));
        }
        let granted = perms.is_some_and(|perms| perms.grant(service) == Grant::Granted);
        rows = rows.child(
            row()
                .w_full()
                .px(s.s3)
                .py(s.s3)
                .gap(s.s3)
                .items_center()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(s.s0_5)
                        .child(
                            text(service.title())
                                .font(t.ty.label.semibold())
                                .color(r.text.text.paint()),
                        )
                        .child(
                            text(service.why())
                                .font(t.ty.caption)
                                .color(r.text.dim.paint())
                                .wrap(),
                        ),
                )
                .child(if granted {
                    good("Granted", t)
                } else {
                    quiet(format!("grant-{}", service.id()), "Grant…", t)
                        .on_press(move |d: &mut Desk| d.grant(service))
                }),
        );
    }
    let bundle = desk.onboarding.bundle.as_ref().map(|path| {
        let name = path.file_stem().map_or_else(
            || "phux Server".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        row()
            .w_full()
            .gap(s.s3)
            .items_center()
            .child(
                col()
                    .key("grant-drag")
                    .gap(s.s1)
                    .padding(s.icon_gap)
                    .radius(t.radius.md)
                    .items_center()
                    .cursor(rune::Cursor::Grab)
                    .label(format!("{name}, drag into the list"))
                    .drag_out(|d: &Desk| {
                        d.onboarding
                            .bundle
                            .as_ref()
                            .map(|b| DragItem::new().file(b))
                    })
                    .child(art(desk.badge_image(), GEOMETRY.badge).radius(t.radius.md))
                    .child(text(name).font(t.ty.caption).color(r.text.text.paint())),
            )
            .child(note_left(
                "If phux isn't in the list, drag it in from here. The list updates on its own.",
                t,
            ))
    });
    let panel = col()
        .key("grant-sheet")
        .w_full()
        .max_w(GEOMETRY.sheet_max)
        .padding(s.sheet_pad)
        .gap(s.s4)
        .radius(t.radius.lg)
        .continuous_corners()
        .fill(r.layer.raised.paint())
        .border(s.hairline, r.border.base.paint())
        .on_screen_layout(|d: &mut Desk, at| d.onboarding.dock.sheet_at = at)
        .on_layout(|d: &mut Desk, at| d.onboarding.dock.sheet_in_window = at)
        .transition(t.enter())
        .child(crate::ui::sheet_header(
            "Manage permissions",
            "grant",
            None,
            Desk::close_grant_sheet,
            t,
        ))
        .child(rows)
        .children(bundle);
    crate::ui::scrim(
        col().key("grant-scrim").padding(s.s4).center(),
        "Manage permissions",
        Some(Desk::close_grant_sheet),
        t,
    )
    .child(t.elevate(panel, Tier::Overlay))
}

fn note_left(words: &'static str, t: &Tokens) -> El {
    text(words)
        .font(t.ty.caption)
        .color(t.roles.text.dim.paint())
        .wrap()
        .grow(1.0)
        .min_w(0.0)
}
