//! The window: a session sidebar, the terminal, and a status line.

use rune::tok;
use rune::{Color, Cx, El, TextStyle, Transition, col, el, row, text};

use crate::desk::Desk;
use crate::theme::{Marks, SELECTED_WASH, Selection, Theme, on_fill};

mod documents;
mod galaxy;
mod hive;
mod icons;
mod inbox;
mod kitchen;
mod machines;
mod news;
mod onboarding;
mod overlay;
mod palette;
mod panes;
pub(crate) use panes::DIVIDER;
mod phone;
mod pitch;
mod pitlane;
mod questions;
mod reef;
mod settings;
mod sidebar;
mod spatial;
mod status;
mod tabs;
mod updates;

/// The app chord's modifier as shown beside an action.
pub(crate) const MOD: &str = if cfg!(any(target_os = "macos", target_os = "ios")) {
    "⌘"
} else {
    "Ctrl+Shift+"
};
const SIDEBAR_W: f32 = 236.0;
/// The band the macOS traffic lights sit in when they draw over content.
const TITLEBAR_H: f32 = 38.0;
/// Space between the terminal area's edge and its grid.
pub(crate) const PAD: f32 = 10.0;
/// The gap around and between the terminal cards, and how round they are.
const CARD_GAP: f32 = 8.0;
const CARD_RADIUS: f32 = 12.0;
/// How far a frosted surface blurs what it floats over, in points.
const FROST: f32 = 18.0;
pub(crate) const LABEL: TextStyle = TextStyle::new(12.0, 400.0);
pub(crate) const LABEL_STRONG: TextStyle = TextStyle::new(12.0, 600.0);
pub(crate) const CAPTION: TextStyle = TextStyle::new(11.0, 500.0);
/// A sidebar row's name.
pub(crate) const NAME: TextStyle = TextStyle::new(12.5, 500.0);

pub(crate) struct Ink {
    pub(crate) bg: Color,
    pub(crate) raised: Color,
    pub(crate) sidebar: Color,
    pub(crate) text: Color,
    pub(crate) dim: Color,
    pub(crate) separator: Color,
    pub(crate) selected: Color,
    /// Where you are in the chrome: a row or tab, without the accent's colour.
    pub(crate) current: Color,
    pub(crate) hover: Color,
    pub(crate) accent: Color,
    /// Text and marks on an accent fill.
    pub(crate) on_accent: Color,
    pub(crate) danger: Color,
    /// The rule between the window's regions.
    pub(crate) edge: Color,
    pub(crate) marks: Marks,
    /// The terminal's selection.
    pub(crate) selection: Selection,
    /// The shadow a floating card casts on the window.
    pub(crate) shade: Color,
}

/// A card's shadow: deep on a dark ground, soft on a light one.
fn shade(ground: Color) -> Color {
    let dark = on_fill(ground) == Color::WHITE;
    Color::BLACK.alpha(if dark { 0.38 } else { 0.13 })
}

fn ink(dark: bool, accent: Color, theme: Option<&Theme>) -> Ink {
    if let Some(theme) = theme {
        let roles = theme.roles();
        return Ink {
            bg: crate::theme::color(theme.background),
            raised: theme.raised(),
            sidebar: theme.surface(0.045),
            text: roles.text,
            dim: roles.dim,
            separator: roles.text.alpha(0.13),
            selected: crate::theme::color(theme.accent).alpha(SELECTED_WASH),
            current: roles.text.alpha(0.1),
            hover: roles.text.alpha(0.07),
            accent: roles.accent,
            on_accent: roles.on_accent,
            danger: roles.danger,
            edge: roles.edge,
            marks: roles.marks,
            selection: roles.selection,
            shade: shade(crate::theme::color(theme.background)),
        };
    }
    let t = tok::theme(dark);
    // Neutral, so no chrome tints the terminal's own colours.
    let (sidebar, raised, edge) = if dark {
        (0x141517, 0x1e1f22, 0x2b2c30)
    } else {
        (0xf1f1f2, 0xfbfbfc, 0xdcdcdf)
    };
    Ink {
        bg: t.bg,
        raised: crate::theme::color(raised),
        sidebar: crate::theme::color(sidebar),
        text: t.text,
        dim: t.text_secondary,
        separator: t.separator,
        selected: accent.alpha(SELECTED_WASH),
        current: t.text.alpha(0.1),
        hover: t.hover,
        accent,
        on_accent: on_fill(accent),
        danger: Theme::system_danger(dark),
        edge: crate::theme::color(edge),
        marks: Theme::system_marks(dark),
        selection: Selection::system(),
        shade: shade(t.bg),
    }
}

pub fn view(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    if desk.phone.is_some() {
        return phone::view(desk, cx);
    }
    let p = ink(cx.system_dark(), cx.accent(), desk.settings.theme);
    // Native fields inherit a readable caret and selection in light themes too.
    cx.foreground(p.text);
    // `titlebar_inset` is the traffic lights' width; zero when the system
    // draws its own titlebar.
    let lights = cx.titlebar_inset();
    let band = if lights > 0.0 { TITLEBAR_H } else { 12.0 };
    if desk.bare.is_some() {
        return bare(desk, &p, lights, band, cx);
    }
    let main = row()
        .key("main")
        .w_full()
        .h_full()
        .child(
            el().key("sidebar-host")
                .w(if desk.sidebar_open { SIDEBAR_W } else { 0.0 })
                .h_full()
                .clip_rounded(0.0)
                .motion(tok::motion::SNAPPY)
                .child(sidebar::view(desk, &p, TITLEBAR_H)),
        )
        .child(
            col()
                .grow(1.0)
                .h_full()
                .min_w(0.0)
                .min_h(0.0)
                .child(status::title_bar(desk, &p, lights))
                .child(
                    col()
                        .grow(1.0)
                        .w_full()
                        .min_w(0.0)
                        .min_h(0.0)
                        .pl(if desk.sidebar_open { 0.0 } else { CARD_GAP })
                        .pr(CARD_GAP)
                        .pb(CARD_GAP)
                        .child(
                            // What floats here never takes rows from a terminal.
                            col()
                                .key("stage")
                                .grow(1.0)
                                .w_full()
                                .min_w(0.0)
                                .min_h(0.0)
                                .child(onboarding::stage(desk, &p, cx))
                                .children(status::confirm_bar(desk, &p))
                                .children(overlay::toast(desk, &p)),
                        ),
                ),
        );
    // No root paste hook: it would read the clipboard before anything here
    // saw the key. ⌘V and Edit ▸ Paste come to `Desk::key` instead, which
    // answers to the modal and keeps the terminal asked for.
    let root = el()
        .w_full()
        .h_full()
        .fill(p.sidebar)
        .child(main)
        .child(crate::host_color::edge(desk.machines.label(), p.sidebar));
    floating(root, desk, &p, band, cx)
}

/// What floats over the window: banners, cards, sheets and menus.
fn floating(root: El, desk: &Desk, p: &Ink, band: f32, cx: &mut Cx<Desk>) -> El {
    root.children(status::banner(desk, p))
        .children(updates::card(desk, p, band))
        .children(desk.inbox.open.then(|| inbox::view(desk, p)))
        .children(playground(desk, p, cx))
        .children(desk.settings_open.then(|| settings::view(desk, p)))
        .children(machines::sheet(desk, p))
        .children(desk.news.open.then(|| news::view(desk, p)))
        .children(tabs::rename_sheet(desk, p))
        .children(desk.palette_open.then(|| palette::view(desk, p, 96.0)))
        .children(
            desk.pointer
                .menu
                .map(|at| palette::context_menu(desk, at, p)),
        )
}

/// A playground view: whether it is open, how it draws, how it closes.
type View = (bool, fn(&Desk, &Ink, &mut Cx<Desk>) -> El, fn(&mut Desk));

/// The open playground view, if any, with the way out every one of them
/// shows. A new view is one more row here.
fn playground(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> Option<El> {
    let views: [View; 6] = [
        (desk.galaxy.open, galaxy::view, Desk::close_galaxy),
        (desk.reef.open, reef::view, Desk::close_reef),
        (desk.pitch.open, pitch::view, Desk::close_pitch),
        (desk.kitchen.open, kitchen::view, Desk::close_kitchen),
        (desk.pitlane.open, pitlane::view, Desk::close_pitlane),
        (desk.hive.open, hive::view, Desk::close_hive),
    ];
    let (_, view, close) = views.into_iter().find(|v| v.0)?;
    // A child of the view: the view is modal, so anything beside it is inert.
    Some(view(desk, p, cx).child(esc_chip(p, close)))
}

/// A frosted key cap, top right in the titlebar band clear of every view's
/// own chrome: `esc` closes this. Escape lets go of a pick first; a press
/// here always closes.
fn esc_chip(p: &Ink, close: fn(&mut Desk)) -> El {
    const KEY: TextStyle = TextStyle::new(10.5, 600.0).mono();
    pressable("playground-esc", p)
        .pin(None, Some(9.0), Some(16.0), None)
        .h(24.0)
        .pl(4.0)
        .pr(9.0)
        .gap(6.0)
        .items_center()
        .radius(8.0)
        .continuous_corners()
        .backdrop_blur(FROST)
        .fill(p.raised.alpha(0.72))
        .border(1.0, p.text.alpha(0.14))
        .shadow(4.0, 14.0, p.shade)
        .hover(p.text.alpha(0.08))
        .label("Close, Escape")
        .on_press(close)
        .child(
            el().h(16.0)
                .px(5.0)
                .radius(4.0)
                .center()
                .fill(p.text.alpha(0.07))
                .border(1.0, p.text.alpha(0.22))
                .child(text("esc").font(KEY).color(p.text).a11y_hidden()),
        )
        .child(text("close").font(CAPTION).color(p.dim).a11y_hidden())
}

/// The bare window: the pane edge to edge under a band the traffic lights
/// and a drag can use, with the same sheets over it.
fn bare(desk: &Desk, p: &Ink, lights: f32, band_h: f32, cx: &mut Cx<Desk>) -> El {
    let band = el()
        .key("titlebar")
        .w_full()
        .h(if lights > 0.0 { TITLEBAR_H } else { 0.0 })
        .window_drag();
    let stage = col()
        .key("stage")
        .grow(1.0)
        .w_full()
        .min_w(0.0)
        .min_h(0.0)
        .child(panes::view(desk, p, cx))
        .children(status::confirm_bar(desk, p))
        .children(overlay::toast(desk, p));
    let main = col().key("main").w_full().h_full().child(band).child(stage);
    let root = el()
        .w_full()
        .h_full()
        .fill(p.bg)
        .child(main)
        .child(crate::host_color::edge(desk.machines.label(), p.bg));
    floating(root, desk, p, band_h, cx)
}

/// A modal sheet: scrim, header with a close button, scrolling body, footer.
pub(crate) struct Sheet {
    pub(crate) key: &'static str,
    pub(crate) title: &'static str,
    pub(crate) width: f32,
    /// Where the sheet sits when the window has room for that.
    pub(crate) top: f32,
    /// How dark the scrim is.
    pub(crate) scrim: f32,
    pub(crate) close: fn(&mut Desk),
}

impl Sheet {
    /// `body` scrolls when the window can't hold it; the header and `foot`
    /// always show, down to the smallest window.
    pub(crate) fn build(self, aside: Option<El>, body: El, foot: El, p: &Ink) -> El {
        const MARGIN: f32 = 16.0;
        const MAX_H: f32 = 620.0;
        let line = || el().w_full().h(1.0).fill(p.separator);
        let header = row()
            .w_full()
            .h(44.0)
            .pl(18.0)
            .pr(10.0)
            .gap(10.0)
            .items_center()
            .child(text(self.title).font(LABEL_STRONG).color(p.text))
            .child(el().grow(1.0))
            .children(aside)
            .child(overlay::icon_button(
                "✕",
                "Close",
                format!("{}-close", self.key),
                p,
                self.close,
            ));
        let panel = col()
            .key(self.key)
            // A narrow window (the phone form) takes what it has.
            .w_full()
            .max_w(self.width)
            .max_h(MAX_H)
            .shrink()
            .min_h(0.0)
            .radius(12.0)
            .clip_rounded(12.0)
            .fill(p.raised)
            .border(1.0, p.separator)
            .shadow(18.0, 56.0, Color::BLACK.alpha(0.5))
            .transition(Transition::scale(0.97).combined(Transition::opacity()))
            .child(header)
            .child(line())
            .child(
                col()
                    .w_full()
                    .shrink()
                    .min_h(0.0)
                    .scroll(rune::ScrollSpec {
                        stick_bottom: false,
                    })
                    .child(body),
            )
            .child(line())
            .child(el().w_full().px(14.0).py(10.0).child(foot));
        col()
            .key(format!("{}-scrim", self.key))
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .py(MARGIN)
            .px(MARGIN)
            .items_center()
            .fill(Color::BLACK.alpha(self.scrim))
            .transition(Transition::opacity())
            .modal(true, self.close)
            .label(self.title)
            .child(el().grow(1.0).max_h(self.top - MARGIN))
            .child(panel)
    }
}

/// A quiet square button around an icon; `name` is what a screen reader
/// says for it.
pub(crate) fn icon_press(
    glyph: &'static rune::Icon,
    name: &'static str,
    key: impl Into<String>,
    p: &Ink,
    press: impl Fn(&mut Desk) + 'static,
) -> El {
    pressable(key, p)
        .size(24.0, 24.0)
        .radius(6.0)
        .center()
        .label(name)
        .on_press(press)
        .child(rune::icon(glyph, 14.0).color(p.dim))
}

/// Something to press: the hover wash and the pointer. Key errors name the
/// caller, not this.
#[track_caller]
pub(crate) fn pressable(key: impl Into<String>, p: &Ink) -> El {
    el().key(key).hover(p.hover).cursor(rune::Cursor::Pointer)
}

pub(crate) fn button(label: &'static str, key: &'static str, p: &Ink, press: fn(&mut Desk)) -> El {
    pressable(key, p)
        .grow(1.0)
        .py(6.0)
        .radius(6.0)
        .center()
        .border(1.0, p.separator)
        .on_press(press)
        .child(text(label).font(CAPTION).color(p.text))
}

/// `on_drag` for a surface floating over panes. Taking the press stops it,
/// and the wheel with it, reaching the pane underneath, without calling the
/// surface a button as `on_press` would.
pub(crate) fn keep_from_pane(_: &mut Desk, _: &mut rune::Drag, _: &mut rune::Tx) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DIM_BAR, TEXT_BAR, contrast};

    #[test]
    fn the_bare_window_drops_the_chrome_and_keeps_the_sheets() {
        let mut desk = Desk::settings_test_fixture();
        desk.settings_open = false;
        let mut rt = rune::Runtime::new(desk, view, (900.0, 600.0), rune::Env::new(false, 1.0));
        rt.frame(0.0);
        assert!(rt.presented("sidebar-toggle").is_some());
        rt.update(|d, _| d.bare = Some(crate::desk::bare::Bare::test("tui", "work")));
        rt.frame(1.0);
        assert!(rt.presented("sidebar-toggle").is_none(), "no title bar");
        assert!(rt.presented("sidebar-host").is_none(), "no sidebar");
        let stage = rt.presented("stage").expect("the pane").0;
        assert!(stage[2] >= 900.0 - 0.5, "edge to edge: {stage:?}");
        rt.update(|d, _| d.settings_open = true);
        rt.frame(2.0);
        assert!(
            rt.presented("settings-close").is_some(),
            "sheets still show"
        );
        // With nothing connected, the chord again simply leaves.
        rt.update(|d, _| d.toggle_bare());
        rt.frame(3.0);
        assert!(rt.presented("sidebar-toggle").is_some());
    }

    #[test]
    fn the_system_appearance_clears_the_bars_too() {
        for dark in [true, false] {
            let p = ink(dark, rune::oklch(0.6, 0.15, 250.0), None);
            let marks = p.marks;
            let readable = [
                ("text", p.text),
                ("danger", p.danger),
                ("alert", marks.alert),
                ("done", marks.done),
                ("bell", marks.bell),
                ("working", marks.working),
                ("good", marks.good),
            ];
            for ground in [p.bg, p.sidebar, p.raised] {
                assert!(contrast(p.dim, ground) >= DIM_BAR, "dim, dark {dark}");
                for (role, ink) in readable {
                    let got = contrast(ink, ground);
                    assert!(got >= TEXT_BAR, "{role}, dark {dark}: {got}");
                }
            }
        }
    }
}
