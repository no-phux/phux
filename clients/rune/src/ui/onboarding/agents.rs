//! The Agents & permissions step and its grant sheet.

use super::*;

/// One of the Agents step's two cards.
fn card(key: &'static str, glyph: &'static rune::Icon, title: &'static str, p: &Ink) -> El {
    col()
        .key(key)
        .w(CARD_W)
        .min_h(220.0)
        .padding(18.0)
        .gap(10.0)
        .items_center()
        .radius(14.0)
        .continuous_corners()
        .fill(p.raised.alpha(0.6))
        .border(1.0, p.separator)
        .child(tile(glyph, 44.0, p))
        .child(
            text(title)
                .font(TextStyle::new(14.0, 650.0))
                .color(p.text)
                .align(Align::Center)
                .role("heading")
                .level(2),
        )
}

fn skill_card(desk: &Desk, p: &Ink) -> El {
    let ob = &desk.onboarding;
    let card = card("onboarding-skill", &icons::PUZZLE, "phux agent skill", p)
        .child(badge("Recommended", p.accent))
        .child(note(
            "Teaches your agents to drive phux: sessions, panes and asks.",
            p,
        ))
        .child(el().grow(1.0));
    let skills = match &ob.skills {
        Probe::Asking => return card.child(note("Checking…", p)),
        Probe::Unsupported => return card.child(note(UPDATE, p)),
        Probe::Failed(why) => {
            return card
                .child(note(why.clone(), p))
                .child(quiet("onboarding-install", "Try again", p).on_press(Desk::install_skill));
        }
        Probe::Ready(skills) => skills,
    };
    if ob.installing {
        return card.child(note("Installing…", p));
    }
    // With no agent installed yet, the shared skill setup writes still counts.
    let mut wanted: Vec<_> = skills.iter().filter(|s| s.detected).collect();
    if wanted.is_empty() {
        wanted = skills.iter().filter(|s| s.installed()).collect();
    }
    let all = !wanted.is_empty() && wanted.iter().all(|s| s.installed());
    let rows = wanted.iter().filter(|_| !all).map(|s| {
        row()
            .w_full()
            .gap(6.0)
            .child(
                text(words::agent_name(&s.agent))
                    .font(LABEL_STRONG)
                    .color(p.text),
            )
            .child(el().grow(1.0))
            .child(
                text(s.label())
                    .font(CAPTION)
                    .color(if s.installed() { p.marks.good } else { p.dim })
                    .truncate()
                    .help(s.hook_detail().unwrap_or_default()),
            )
    });
    let card = card.children(rows);
    if all {
        card.child(good("Installed", p))
    } else {
        card.child(
            pill(
                "onboarding-install",
                "Install".into(),
                p.accent,
                p.on_accent,
                p,
            )
            .on_press(Desk::install_skill),
        )
    }
}

/// Opt-in: it adds a line to the shell rc, so it is never the accent.
fn shim_card(desk: &Desk, p: &Ink) -> El {
    let card = card("onboarding-shims", &icons::WINDOW, "Open agents in phux", p)
        .child(note(
            "Plain claude, codex, opencode and pi start inside phux. Adds one line to your shell config.",
            p,
        ))
        .child(el().grow(1.0));
    match &desk.onboarding.shims {
        None => card.child(quiet("onboarding-shim", "Set up", p).on_press(Desk::install_shims)),
        Some(Probe::Asking) => card.child(note("Setting up…", p)),
        Some(Probe::Unsupported) => card.child(note(UPDATE, p)),
        Some(Probe::Failed(why)) => card
            .child(note(why.clone(), p))
            .child(quiet("onboarding-shim", "Try again", p).on_press(Desk::install_shims)),
        Some(Probe::Ready(agents)) if agents.is_empty() => {
            card.child(note("No agents found on this computer.", p))
        }
        Some(Probe::Ready(agents)) => card
            .child(note(
                format!("{}. Open a new terminal to use them.", agents.join(", ")),
                p,
            ))
            .child(good("Set up", p)),
    }
}

fn permissions_card(desk: &Desk, p: &Ink) -> El {
    let card = card(
        "onboarding-permissions",
        &icons::HAND,
        "System permissions",
        p,
    );
    let card = match &desk.onboarding.permissions {
        Probe::Asking => card.child(badge("Checking…", p.dim)),
        Probe::Ready(perms) => {
            let granted = perms.granted();
            let ink = if granted == Service::ALL.len() {
                p.marks.good
            } else {
                p.dim
            };
            card.child(badge(
                format!("{granted} of {} granted", Service::ALL.len()),
                ink,
            ))
        }
        Probe::Unsupported | Probe::Failed(_) => card,
    };
    let card = card
        .child(note("Some agent work needs macOS to say yes first.", p))
        .child(el().grow(1.0));
    match &desk.onboarding.permissions {
        Probe::Unsupported => card.child(note(UPDATE, p)),
        Probe::Failed(why) => card.child(note(why.clone(), p)),
        Probe::Ready(perms) if perms.granted() == Service::ALL.len() => {
            card.child(good("All granted", p))
        }
        _ => card.child(quiet("onboarding-grant", "Grant…", p).on_press(Desk::open_grant_sheet)),
    }
}

pub(super) fn agents(desk: &Desk, p: &Ink) -> El {
    col()
        .w_full()
        .items_center()
        .gap(18.0)
        .child(heading(
            &icons::PUZZLE,
            "Agents & permissions",
            "Give your agents the phux skill, and phux the access they ask for.",
            p,
        ))
        .child(
            row()
                .gap(14.0)
                .flex_wrap()
                .justify_center()
                .child(skill_card(desk, p))
                .child(shim_card(desk, p))
                .children(PERMISSIONS.then(|| permissions_card(desk, p))),
        )
}

/// The sheet over the Agents step: each permission, why, and its pane.
pub(super) fn grant_sheet(desk: &Desk, p: &Ink) -> El {
    let perms = desk.onboarding.permissions.ready();
    let mut rows = col()
        .w_full()
        .radius(12.0)
        .fill(p.bg)
        .border(1.0, p.separator);
    for (at, service) in Service::ALL.into_iter().enumerate() {
        if at > 0 {
            rows = rows.child(el().w_full().h(1.0).fill(p.separator));
        }
        let granted = perms.is_some_and(|perms| perms.grant(service) == Grant::Granted);
        rows = rows.child(
            row()
                .w_full()
                .px(14.0)
                .py(12.0)
                .gap(12.0)
                .items_center()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(3.0)
                        .child(text(service.title()).font(LABEL_STRONG).color(p.text))
                        .child(text(service.why()).font(COPY).color(p.dim).wrap()),
                )
                .child(if granted {
                    good("Granted", p)
                } else {
                    quiet(format!("grant-{}", service.id()), "Grant…", p)
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
            .gap(12.0)
            .items_center()
            .child(
                col()
                    .key("grant-drag")
                    .gap(4.0)
                    .padding(6.0)
                    .radius(12.0)
                    .items_center()
                    .cursor(rune::Cursor::Grab)
                    .label(format!("{name}, drag into the list"))
                    .drag_out(|d: &Desk| {
                        d.onboarding
                            .bundle
                            .as_ref()
                            .map(|b| DragItem::new().file(b))
                    })
                    .child(art(desk.badge_image(), 44.0).radius(10.0))
                    .child(text(name).font(CAPTION).color(p.text)),
            )
            .child(note_left(
                "If phux isn't in the list, drag it in from here. The list updates on its own.",
                p,
            ))
    });
    let panel = col()
        .key("grant-sheet")
        .w_full()
        .max_w(480.0)
        .padding(18.0)
        .gap(14.0)
        .radius(16.0)
        .continuous_corners()
        .fill(p.raised)
        .border(1.0, p.separator)
        .shadow(18.0, 56.0, p.shade)
        .on_screen_layout(|d: &mut Desk, at| d.onboarding.dock.sheet_at = at)
        .on_layout(|d: &mut Desk, at| d.onboarding.dock.sheet_in_window = at)
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .child(
            row()
                .w_full()
                .items_center()
                .child(
                    text("Manage permissions")
                        .font(TextStyle::new(18.0, 700.0))
                        .color(p.text)
                        .role("heading")
                        .level(2),
                )
                .child(el().grow(1.0))
                .child(crate::ui::overlay::icon_button(
                    &crate::ui::icons::CLOSE,
                    "Close",
                    "grant-close",
                    p,
                    Desk::close_grant_sheet,
                )),
        )
        .child(rows)
        .children(bundle);
    col()
        .key("grant-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .padding(16.0)
        .center()
        .fill(Color::BLACK.alpha(0.28))
        .transition(Transition::opacity())
        .modal(true, Desk::close_grant_sheet)
        .label("Manage permissions")
        .child(panel)
}

fn note_left(words: &'static str, p: &Ink) -> El {
    text(words)
        .font(COPY)
        .color(p.dim)
        .wrap()
        .grow(1.0)
        .min_w(0.0)
}
