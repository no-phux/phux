//! The window: a session sidebar, the terminal, and a status line.

use rune::tok;
use rune::{Color, Cx, El, TextStyle, col, el, row, text};

use crate::desk::Desk;
use crate::theme::Theme;

mod inbox;
mod machines;
mod overlay;
mod palette;
mod panes;
pub(crate) use panes::DIVIDER;
mod settings;
mod sidebar;
mod status;
mod tabs;
mod updates;

/// The app chord's modifier as shown beside an action.
pub(crate) const MOD: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl+Shift+"
};
const SIDEBAR_W: f32 = 236.0;
/// The band the macOS traffic lights sit in when they draw over content.
const TITLEBAR_H: f32 = 38.0;
/// Space between the terminal area's edge and its grid.
pub(crate) const PAD: f32 = 6.0;
pub(crate) const LABEL: TextStyle = TextStyle::new(12.0, 400.0);
pub(crate) const LABEL_STRONG: TextStyle = TextStyle::new(12.0, 600.0);
pub(crate) const CAPTION: TextStyle = TextStyle::new(11.0, 500.0);

pub(crate) struct Ink {
    pub(crate) bg: Color,
    pub(crate) raised: Color,
    pub(crate) sidebar: Color,
    pub(crate) text: Color,
    pub(crate) dim: Color,
    pub(crate) separator: Color,
    pub(crate) selected: Color,
    pub(crate) hover: Color,
    pub(crate) accent: Color,
    /// Text and marks on an accent fill.
    pub(crate) on_accent: Color,
    pub(crate) danger: Color,
    /// The rule between the window's regions.
    pub(crate) edge: Color,
}

fn ink(dark: bool, accent: Color, theme: Option<&Theme>) -> Ink {
    if let Some(theme) = theme {
        let accent = crate::theme::color(theme.accent);
        let text = crate::theme::color(theme.foreground);
        return Ink {
            bg: crate::theme::color(theme.background),
            raised: theme.surface(0.09),
            sidebar: theme.surface(0.045),
            text,
            dim: text.alpha(0.62),
            separator: text.alpha(0.13),
            selected: accent.alpha(0.22),
            hover: text.alpha(0.07),
            accent,
            on_accent: Color::WHITE,
            danger: crate::theme::color(theme.ansi[1]),
            edge: text.alpha(0.13),
        };
    }
    let t = tok::theme(dark);
    Ink {
        bg: t.bg,
        raised: t.card,
        sidebar: t.card,
        text: t.text,
        dim: t.text_secondary,
        separator: t.separator,
        selected: accent.alpha(0.22),
        hover: t.hover,
        accent,
        on_accent: Color::WHITE,
        danger: rune::oklch(0.68, 0.17, 25.0),
        edge: t.separator,
    }
}

pub fn view(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let p = ink(cx.system_dark(), cx.accent(), desk.settings.theme);
    // `titlebar_inset` is the traffic lights' width; zero when the system
    // draws its own titlebar.
    let lights = cx.titlebar_inset();
    let band = if lights > 0.0 { TITLEBAR_H } else { 12.0 };
    // With the sidebar folded, the traffic lights sit over the terminal's
    // top edge: a draggable titlebar that says what is on screen.
    let top = (!desk.sidebar_open).then(|| {
        row()
            .key("titlebar")
            .w_full()
            .h(band)
            .pl(lights + 8.0)
            .pr(lights + 8.0)
            .center()
            .window_drag()
            .child(
                text(desk.window_title())
                    .font(CAPTION)
                    .color(p.dim)
                    .truncate(),
            )
    });
    let main = row()
        .key("main")
        .w_full()
        .h_full()
        .child(
            el().key("sidebar-host")
                .w(if desk.sidebar_open {
                    SIDEBAR_W + 1.0
                } else {
                    0.0
                })
                .h_full()
                .clip_rounded(0.0)
                .motion(tok::motion::SNAPPY)
                .child(
                    row()
                        .w(SIDEBAR_W + 1.0)
                        .h_full()
                        .child(sidebar::view(desk, &p, band))
                        .child(el().w(1.0).h_full().fill(p.edge)),
                ),
        )
        .child(
            col()
                .grow(1.0)
                .h_full()
                .min_w(0.0)
                .min_h(0.0)
                .children(top)
                .children(tabs::strip(desk, &p))
                .children(status::confirm_bar(desk, &p))
                .child(panes::view(desk, &p, cx))
                .child(el().w_full().h(1.0).fill(p.edge))
                .child(status::status_line(desk, &p)),
        );
    el().w_full()
        .h_full()
        .fill(p.bg)
        // Rune hands the root pastes nothing focused took (Edit ▸ Paste).
        .on_paste_content(|desk: &mut Desk, content, _| desk.paste_content(content))
        .child(main)
        .children(status::banner(desk, &p))
        .children(updates::card(desk, &p, band))
        .children(desk.inbox.open.then(|| inbox::view(desk, &p)))
        .children(desk.settings_open.then(|| settings::view(desk, &p)))
        .children(machines::sheet(desk, &p))
        .children(tabs::rename_sheet(desk, &p))
        .children(desk.palette_open.then(|| palette::view(desk, &p)))
        .children(desk.pointer.menu.map(|at| palette::context_menu(desk, at, &p)))
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
