//! First-run onboarding, drawn where the terminals go: the sidebar and title
//! bar stay, so every pick on the Look step shows on the real window. While
//! an agent brings a setup over, its pane sits beside the step.

use rune::{Align, Color, Cx, DragItem, El, TextStyle, Transition, col, el, row, text};

use super::{CAPTION, CARD_GAP, Ink, LABEL, LABEL_STRONG, icons, panes, pressable};
use crate::desk::Desk;
use crate::desk::onboarding::{Migrate, Probe};
use crate::keys::{self, Shortcut};
use crate::onboarding::{self as words, Grant, PERMISSIONS, Service, Step};
use crate::theme::{self, Theme};

const TITLE: TextStyle = TextStyle::new(26.0, 700.0);
const LEAD: TextStyle = TextStyle::new(13.5, 450.0).lh(19.0);
const COPY: TextStyle = TextStyle::new(12.0, 450.0).lh(16.0);
const CARD_W: f32 = 236.0;

/// The terminal area: the step when onboarding is open, the panes otherwise,
/// and both while the migration's agent works.
pub fn stage(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    if !desk.onboarding.open || desk.bare.is_some() {
        return panes::view(desk, p, cx);
    }
    let step = panes::card(
        el().w_full().h_full().fill(p.bg).child(page(desk, p)),
        p.bg,
        panes::Lit::Resting,
        p,
    );
    if !desk.onboarding.migrating() {
        return el()
            .key("onboarding")
            .grow(1.0)
            .w_full()
            .min_h(0.0)
            .child(step);
    }
    row()
        .key("onboarding")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .gap(CARD_GAP)
        .child(el().w(0.0).grow(0.8).min_w(0.0).h_full().child(step))
        .child(
            col()
                .w(0.0)
                .grow(1.2)
                .min_w(0.0)
                .h_full()
                .child(panes::view(desk, p, cx)),
        )
}

/// One step, centred while it fits and scrolling when it doesn't.
fn page(desk: &Desk, p: &Ink) -> El {
    let step = desk.onboarding.step;
    let body = match step {
        Step::Welcome => welcome(p),
        Step::Look => look(desk, p),
        Step::Agents => agents(desk, p),
        Step::Migrate => migrate(desk, p),
        Step::Done => done(p),
    };
    let content = col()
        .key(format!("onboarding-{}", step.id()))
        .w_full()
        .shrink()
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(
            col()
                .w_full()
                .px(24.0)
                .py(28.0)
                .gap(14.0)
                .items_center()
                .transition(Transition::opacity())
                .child(body)
                .children(nav(desk, p)),
        );
    col()
        .w_full()
        .h_full()
        .role("dialog")
        .label("Get started with phux")
        .child(el().grow(1.0))
        .child(content)
        .child(el().grow(1.0))
        .children(desk.onboarding.grant_sheet.then(|| grant_sheet(desk, p)))
}

fn heading(glyph: &'static rune::Icon, title: &'static str, lead: &'static str, p: &Ink) -> El {
    col()
        .w_full()
        .max_w(560.0)
        .gap(8.0)
        .items_center()
        .child(tile(glyph, 64.0, p))
        .child(
            text(title)
                .font(TITLE)
                .color(p.text)
                .align(Align::Center)
                .role("heading")
                .level(1),
        )
        .child(
            text(lead)
                .font(LEAD)
                .color(p.dim)
                .align(Align::Center)
                .wrap(),
        )
}

/// An icon on a rounded accent tile.
fn tile(glyph: &'static rune::Icon, size: f32, p: &Ink) -> El {
    el().size(size, size)
        .radius(size * 0.25)
        .continuous_corners()
        .fill(p.accent.alpha(0.16))
        .border(1.0, p.accent.alpha(0.28))
        .center()
        .child(rune::icon(glyph, size * 0.5).color(p.accent))
        .a11y_hidden()
}

/// The accent pill: Continue, Install, Start.
fn primary(key: &'static str, label: &'static str, p: &Ink, press: fn(&mut Desk)) -> El {
    pressable(key, p)
        .h(34.0)
        .px(18.0)
        .gap(8.0)
        .radius(17.0)
        .items_center()
        .fill(p.accent)
        .focusable(true)
        .role("button")
        .label(label)
        .on_press(press)
        .child(
            text(label)
                .font(TextStyle::new(14.0, 600.0))
                .color(p.on_accent)
                .a11y_hidden(),
        )
        .child(
            rune::icon(&icons::ARROW_RIGHT, 14.0)
                .color(p.on_accent)
                .a11y_hidden(),
        )
}

/// A quiet pill.
fn quiet(key: impl Into<String>, label: impl Into<String>, p: &Ink) -> El {
    pill(key, label.into(), p.current, p.text, p)
}

fn pill(key: impl Into<String>, label: String, fill: Color, ink: Color, p: &Ink) -> El {
    pressable(key, p)
        .h(28.0)
        .px(12.0)
        .radius(14.0)
        .items_center()
        .fill(fill)
        .focusable(true)
        .role("button")
        .label(label.clone())
        .child(text(label).font(LABEL_STRONG).color(ink).a11y_hidden())
}

/// A rounded word on a tinted ground: Recommended, 1 of 3 granted.
fn badge(words: impl Into<String>, ink: Color) -> El {
    el().px(8.0)
        .py(2.0)
        .radius(9.0)
        .border(1.0, ink.alpha(0.5))
        .fill(ink.alpha(0.1))
        .child(text(words.into()).font(CAPTION).color(ink))
        .self_center()
}

/// A green check and a word: Installed, Granted.
fn good(word: &'static str, p: &Ink) -> El {
    row()
        .h(28.0)
        .px(12.0)
        .gap(6.0)
        .radius(14.0)
        .items_center()
        .fill(p.marks.good.alpha(0.12))
        .child(rune::icon(&icons::CHECK, 13.0).color(p.marks.good))
        .child(text(word).font(LABEL_STRONG).color(p.marks.good))
}

/// Back and Continue, under every step but the first and last.
fn nav(desk: &Desk, p: &Ink) -> Option<El> {
    let step = desk.onboarding.step;
    if matches!(step, Step::Welcome | Step::Done) {
        return None;
    }
    let back = pressable("onboarding-back", p)
        .size(34.0, 34.0)
        .radius(17.0)
        .center()
        .fill(p.accent.alpha(0.14))
        .focusable(true)
        .label("Back")
        .on_press(Desk::onboarding_back)
        .child(
            rune::icon(&icons::ARROW_LEFT, 14.0)
                .color(p.accent)
                .a11y_hidden(),
        );
    Some(
        row()
            .pt(10.0)
            .gap(10.0)
            .items_center()
            .child(back)
            .child(primary(
                "onboarding-continue",
                "Continue",
                p,
                Desk::onboarding_continue,
            )),
    )
}

fn welcome(p: &Ink) -> El {
    col()
        .w_full()
        .max_w(520.0)
        .gap(12.0)
        .items_center()
        .child(rune::icon(&icons::FOX, 112.0).color(p.text).a11y_hidden())
        .child(
            text("Get started with phux")
                .font(TextStyle::new(30.0, 700.0))
                .color(p.text)
                .align(Align::Center)
                .role("heading")
                .level(1),
        )
        .child(
            text(
                "Pick a look, give your agents the phux skill and the permissions they \
                 need, and bring your key bindings with you.",
            )
            .font(LEAD)
            .color(p.dim)
            .align(Align::Center)
            .wrap(),
        )
        .child(el().h(6.0))
        .child(primary(
            "onboarding-continue",
            "Continue",
            p,
            Desk::onboarding_continue,
        ))
        .child(
            pressable("onboarding-skip", p)
                .px(10.0)
                .py(4.0)
                .radius(6.0)
                .focusable(true)
                .label("Skip for now")
                .on_press(Desk::skip_onboarding)
                .child(
                    text("Skip for now")
                        .font(LABEL)
                        .color(p.accent)
                        .a11y_hidden(),
                ),
        )
}

/// A titled group inside a panel.
fn group(title: &'static str, body: El, p: &Ink) -> El {
    col()
        .gap(8.0)
        .min_w(0.0)
        .child(
            text(title)
                .font(LABEL_STRONG)
                .color(p.dim)
                .role("heading")
                .level(2),
        )
        .child(body)
}

fn panel(p: &Ink) -> El {
    col()
        .w_full()
        .max_w(600.0)
        .padding(16.0)
        .gap(16.0)
        .radius(14.0)
        .continuous_corners()
        .fill(p.raised.alpha(0.6))
        .border(1.0, p.separator)
}

/// A pickable tile with a drawing and its word under it.
fn choice(
    key: &'static str,
    word: &'static str,
    on: bool,
    art: El,
    p: &Ink,
    press: impl Fn(&mut Desk) + 'static,
) -> El {
    col()
        .gap(6.0)
        .items_center()
        .child(
            pressable(key, p)
                .radius(12.0)
                .padding(3.0)
                .border(2.0, if on { p.accent } else { Color::TRANSPARENT })
                .focusable(true)
                .role("radio")
                .label(word)
                .checked(on)
                .on_press(press)
                .child(art),
        )
        .child(
            text(word)
                .font(if on { LABEL_STRONG } else { LABEL })
                .color(if on { p.text } else { p.dim })
                .a11y_hidden(),
        )
}

/// A little window: a sidebar strip (when shown) and its traffic lights.
fn window(ground: Color, ink: Color, sidebar: bool) -> El {
    let dots = row()
        .gap(3.0)
        .child(
            el().size(5.0, 5.0)
                .radius(2.5)
                .fill(rune::oklch(0.68, 0.19, 25.0)),
        )
        .child(
            el().size(5.0, 5.0)
                .radius(2.5)
                .fill(rune::oklch(0.82, 0.16, 85.0)),
        )
        .child(
            el().size(5.0, 5.0)
                .radius(2.5)
                .fill(rune::oklch(0.74, 0.17, 145.0)),
        );
    row()
        .size(72.0, 48.0)
        .radius(9.0)
        .clip_rounded(9.0)
        .fill(ground)
        .border(1.0, ink.alpha(0.18))
        .children(sidebar.then(|| el().w(18.0).h_full().fill(ink.alpha(0.1))))
        .child(
            col()
                .grow(1.0)
                .h_full()
                .padding(6.0)
                .gap(5.0)
                .child(dots)
                .child(el().w_full().h(4.0).radius(2.0).fill(ink.alpha(0.25)))
                .child(el().w(24.0).h(4.0).radius(2.0).fill(ink.alpha(0.18))),
        )
}

fn look(desk: &Desk, p: &Ink) -> El {
    let current = desk.settings.theme;
    let scheme = current.map(|t| t.dark);
    let (light, dark) = (theme::color(0xf2f2f4), theme::color(0x1c1c21));
    let auto = row()
        .size(72.0, 48.0)
        .radius(9.0)
        .clip_rounded(9.0)
        .child(window(light, Color::BLACK, false).w(36.0).radius(0.0))
        .child(window(dark, Color::WHITE, false).w(36.0).radius(0.0));
    let schemes = row()
        .gap(10.0)
        .child(choice(
            "look-auto",
            "Auto",
            scheme.is_none(),
            auto,
            p,
            |d: &mut Desk| d.choose_scheme(None),
        ))
        .child(choice(
            "look-light",
            "Light",
            scheme == Some(false),
            window(light, Color::BLACK, false),
            p,
            |d: &mut Desk| d.choose_scheme(Some(false)),
        ))
        .child(choice(
            "look-dark",
            "Dark",
            scheme == Some(true),
            window(dark, Color::WHITE, false),
            p,
            |d: &mut Desk| d.choose_scheme(Some(true)),
        ));
    let shown = desk.sidebar_open;
    let sidebar = row()
        .gap(10.0)
        .child(choice(
            "look-sidebar",
            "Sidebar",
            shown,
            window(p.bg, p.text, true),
            p,
            |d: &mut Desk| {
                if !d.sidebar_open {
                    d.toggle_sidebar();
                }
            },
        ))
        .child(choice(
            "look-no-sidebar",
            "Hidden",
            !shown,
            window(p.bg, p.text, false),
            p,
            |d: &mut Desk| {
                if d.sidebar_open {
                    d.toggle_sidebar();
                }
            },
        ));
    // The picker's own list, System first, narrowed to the chosen scheme.
    let themes = std::iter::once(None)
        .chain(theme::all().map(Some))
        .filter(|t| scheme.is_none_or(|dark| t.is_some_and(|t| t.dark == dark)));
    let strip = row()
        .key("look-themes")
        .w_full()
        .min_w(0.0)
        .scroll_x()
        .scroll_edge_fade(16.0)
        .child(
            row()
                .gap(10.0)
                .py(2.0)
                .children(themes.map(|t| theme_card(t, current, p))),
        );
    col()
        .w_full()
        .items_center()
        .gap(18.0)
        .child(heading(
            &icons::PALETTE,
            "Make phux yours",
            "Every pick shows on this window as you make it.",
            p,
        ))
        .child(
            panel(p)
                .child(
                    row()
                        .w_full()
                        .gap(24.0)
                        .flex_wrap()
                        .child(group("Colour scheme", schemes, p))
                        .child(group("Sidebar", sidebar, p)),
                )
                .child(el().w_full().h(1.0).fill(p.separator))
                .child(group("Theme", strip, p).w_full()),
        )
}

/// A theme as a card in its own colours: `Aa`, a prompt in its accent, and
/// its palette as dots. `None` is System.
fn theme_card(t: Option<&'static Theme>, current: Option<&'static Theme>, p: &Ink) -> El {
    let (ground, ink, accent) = t.map_or((p.bg, p.text, p.accent), |t| {
        (
            theme::color(t.background),
            theme::color(t.foreground),
            theme::color(t.accent),
        )
    });
    let palette: Vec<Color> = t.map_or_else(
        || {
            let m = p.marks;
            vec![m.alert, m.good, m.working, p.accent, m.done, m.bell]
        },
        |t| t.ansi[1..7].iter().map(|c| theme::color(*c)).collect(),
    );
    let on = t.map(|t| t.id) == current.map(|t| t.id);
    let name = t.map_or("System", |t| t.name);
    let art = col()
        .size(112.0, 58.0)
        .padding(8.0)
        .gap(7.0)
        .radius(9.0)
        .fill(ground)
        .border(1.0, ink.alpha(0.16))
        .child(
            row()
                .gap(8.0)
                .items_center()
                .child(text("Aa").font(TextStyle::new(15.0, 600.0)).color(ink))
                .child(
                    el().px(4.0).radius(4.0).fill(accent.alpha(0.22)).child(
                        text("~/")
                            .font(CAPTION.family(rune::Family::SystemMono))
                            .color(accent),
                    ),
                ),
        )
        .child(
            row().gap(3.0).children(
                palette
                    .into_iter()
                    .map(|c| el().size(9.0, 9.0).radius(4.5).fill(c)),
            ),
        );
    let key = format!("look-theme-{}", t.map_or("system", |t| t.id));
    col()
        .gap(5.0)
        .items_center()
        .child(
            pressable(key, p)
                .radius(12.0)
                .padding(3.0)
                .border(2.0, if on { p.accent } else { Color::TRANSPARENT })
                .focusable(true)
                .role("radio")
                .label(name)
                .checked(on)
                .on_press(move |d: &mut Desk| d.choose_theme(t))
                .child(art.a11y_hidden()),
        )
        .child(
            text(name)
                .font(if on { LABEL_STRONG } else { CAPTION })
                .color(if on { p.text } else { p.dim })
                .max_w(118.0)
                .truncate()
                .a11y_hidden(),
        )
}

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

fn note(words: impl Into<String>, p: &Ink) -> El {
    text(words.into())
        .font(COPY)
        .color(p.dim)
        .align(Align::Center)
        .wrap()
}

const UPDATE: &str = "Update phux to enable this.";

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
    let wanted: Vec<_> = skills.iter().filter(|s| s.detected).collect();
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
                text(s.summary())
                    .font(CAPTION)
                    .color(if s.installed() { p.marks.good } else { p.dim })
                    .truncate(),
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

fn agents(desk: &Desk, p: &Ink) -> El {
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
                .children(PERMISSIONS.then(|| permissions_card(desk, p))),
        )
}

/// The sheet over the Agents step: each permission, why, and its pane.
fn grant_sheet(desk: &Desk, p: &Ink) -> El {
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
                    .child(rune::icon(&icons::FOX, 44.0).color(p.text))
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
                .child(super::overlay::icon_button(
                    "✕",
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

fn migrate(desk: &Desk, p: &Ink) -> El {
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
                    "The agent brings over key bindings from tmux, zellij, Ghostty, kitty, \
                     WezTerm, iTerm2 and Alacritty, and asks before replacing one. Themes \
                     and fonts don't move.",
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

fn done(p: &Ink) -> El {
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

#[cfg(test)]
mod tests {
    use rune::accesskit::Action;
    use rune::{Env, Key, KeyEvent, Mods, Runtime};

    use super::*;
    use crate::onboarding::{AgentSkill, Permissions};

    const SIZE: (f32, f32) = (1100.0, 720.0);
    const SCALE: f32 = 2.0;

    fn skill(agent: &str, state: &str) -> AgentSkill {
        AgentSkill {
            agent: agent.to_owned(),
            detected: true,
            skill: Some(state.to_owned()),
            ..AgentSkill::default()
        }
    }

    /// The real window on `step`, as a first run with Claude Code and Codex
    /// found and one permission granted would show it.
    fn window(step: Step) -> Runtime<Desk> {
        let mut desk = Desk::settings_test_fixture();
        desk.settings_open = false;
        desk.settings.theme = theme::find("midnight");
        let mut rt = Runtime::new(desk, crate::ui::view, SIZE, Env::new(true, SCALE));
        rt.on_key = Some(Desk::key);
        rt.update(|d, tx| {
            d.open_onboarding();
            // The offline fixture has no CLI: drop what that answered.
            d.pump_onboarding(tx);
            let ob = &mut d.onboarding;
            ob.skills = Probe::Ready(vec![
                skill("claude", "installed"),
                skill("codex", "missing"),
            ]);
            ob.permissions = Probe::Ready(Permissions {
                responsible_path: Some("/Applications/phux Server.app".into()),
                services: vec![(Service::FullDiskAccess, Grant::Granted)],
            });
            ob.bundle = Some("/Applications/phux Server.app".into());
            ob.step = step;
        });
        settle(&mut rt);
        rt
    }

    fn settle(rt: &mut Runtime<Desk>) {
        for t in [0.0, 0.5, 1.0, 2.0] {
            rt.frame(t);
        }
    }

    fn has(rt: &Runtime<Desk>, key: &str) -> bool {
        rt.find(key).is_some()
    }

    /// `RUNE_ONBOARDING_SNAP=DIR` writes each frame as `DIR/NAME.png`.
    fn snap(rt: &mut Runtime<Desk>, name: &str) {
        let Some(dir) = std::env::var_os("RUNE_ONBOARDING_SNAP") else {
            return;
        };
        let (w, h) = ((SIZE.0 * SCALE) as u32, (SIZE.1 * SCALE) as u32);
        let gpu = rune::render::Gpu::headless();
        let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
        let target = rune::render::Offscreen::new(&gpu.device, w, h);
        renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
        renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut rt.text.atlas,
            &rt.instances,
            (w, h),
        );
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.draw(&mut enc, &target.view, [0.0; 4]);
        target.copy(&mut enc);
        renderer.submit(&gpu.queue, enc);
        let pixels = target.read(&gpu.device);
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        std::fs::create_dir_all(&dir).unwrap();
        rune::render::write_png(&path, w, h, &pixels).unwrap();
    }

    #[test]
    fn every_step_renders_its_parts() {
        let mut rt = window(Step::Welcome);
        assert!(has(&rt, "onboarding-continue") && has(&rt, "onboarding-skip"));
        assert!(!has(&rt, "onboarding-back"), "nothing to go back to");
        snap(&mut rt, "1-welcome");

        let mut rt = window(Step::Look);
        for key in [
            "look-auto",
            "look-light",
            "look-dark",
            "look-sidebar",
            "look-theme-midnight",
        ] {
            assert!(has(&rt, key), "{key}");
        }
        assert!(
            !has(&rt, "look-theme-catppuccin-latte"),
            "dark themes only on Dark"
        );
        snap(&mut rt, "2-look");

        let mut rt = window(Step::Agents);
        assert!(has(&rt, "onboarding-skill") && has(&rt, "onboarding-install"));
        assert_eq!(
            has(&rt, "onboarding-permissions"),
            PERMISSIONS,
            "macOS only"
        );
        snap(&mut rt, "3-agents");
        if PERMISSIONS {
            // One frame per runtime: a snapshot takes the glyph atlas's uploads.
            let mut rt = window(Step::Agents);
            assert!(rt.accessibility_action("onboarding-grant", Action::Click, None));
            settle(&mut rt);
            assert!(!has(&rt, "grant-full_disk_access"), "granted shows Granted");
            assert!(has(&rt, "grant-screen_recording") && has(&rt, "grant-accessibility"));
            assert!(has(&rt, "grant-drag"), "the server's app to drag in");
            snap(&mut rt, "3b-grant-sheet");
        }

        let mut rt = window(Step::Migrate);
        for key in [
            "migrate-claude",
            "migrate-codex",
            "migrate-skip",
            "migrate-copy",
        ] {
            assert!(has(&rt, key), "{key}");
        }
        assert!(
            !has(&rt, "migrate-opencode"),
            "only agents that are installed"
        );
        snap(&mut rt, "4-migrate");
        let mut rt = window(Step::Migrate);
        rt.update(|d, _| d.onboarding.migrate = Migrate::Running("claude".into(), None));
        settle(&mut rt);
        assert!(has(&rt, "migrate-status") && has(&rt, "migrate-cancel"));
        assert!(has(&rt, "terminal"), "the agent's pane beside the step");
        assert_eq!(rt.state.modal(), None, "typing reaches the agent");
        snap(&mut rt, "4b-migrating");

        let mut rt = window(Step::Done);
        for key in ["done-key-0", "done-key-1", "done-key-2", "onboarding-start"] {
            assert!(has(&rt, key), "{key}");
        }
        snap(&mut rt, "5-done");
    }

    #[test]
    fn picks_apply_to_the_window_and_keys_move_on() {
        let mut rt = window(Step::Look);
        assert!(rt.accessibility_action("look-light", Action::Click, None));
        rt.frame(3.0);
        assert!(
            rt.state.settings.theme.is_some_and(|t| !t.dark),
            "a light theme, live"
        );
        assert!(rt.accessibility_action("look-auto", Action::Click, None));
        rt.frame(3.5);
        assert_eq!(rt.state.settings.theme, None, "Auto follows the system");
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(4.0);
        assert_eq!(rt.state.onboarding.step, Step::Agents);
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(4.5);
        assert_eq!(rt.state.onboarding.step, Step::Look);
    }

    #[test]
    fn start_using_phux_closes_it() {
        let mut rt = window(Step::Done);
        assert!(rt.accessibility_action("onboarding-start", Action::Click, None));
        settle(&mut rt);
        assert!(!rt.state.onboarding.open);
        assert!(!has(&rt, "onboarding"));
    }
}
