//! The Look step: colour scheme, sidebar, and the theme strip.

use super::*;

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

pub(super) fn look(desk: &Desk, p: &Ink) -> El {
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
            |d: &mut Desk| d.set_sidebar_open(true),
        ))
        .child(choice(
            "look-no-sidebar",
            "Hidden",
            !shown,
            window(p.bg, p.text, false),
            p,
            |d: &mut Desk| d.set_sidebar_open(false),
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
