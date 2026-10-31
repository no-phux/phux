//! The window: a session sidebar, the terminal, and a status line.

use rune::tok;
use rune::{Color, Cx, El, TextStyle, Transition, col, el, row, text};

use crate::desk::Desk;
use crate::theme::{Marks, SELECTED_WASH, Selection, Theme, on_fill};

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
    pub(crate) marks: Marks,
    /// The terminal's selection.
    pub(crate) selection: Selection,
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
            hover: roles.text.alpha(0.07),
            accent: roles.accent,
            on_accent: roles.on_accent,
            danger: roles.danger,
            edge: roles.edge,
            marks: roles.marks,
            selection: roles.selection,
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
        selected: accent.alpha(SELECTED_WASH),
        hover: t.hover,
        accent,
        on_accent: on_fill(accent),
        danger: Theme::system_danger(dark),
        edge: t.separator,
        marks: Theme::system_marks(dark),
        selection: Selection::system(),
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
                .child(
                    // What floats here never takes rows from a terminal.
                    col()
                        .key("stage")
                        .grow(1.0)
                        .w_full()
                        .min_w(0.0)
                        .min_h(0.0)
                        .child(panes::view(desk, &p, cx))
                        .children(status::confirm_bar(desk, &p))
                        .children(overlay::toast(desk, &p)),
                )
                .child(el().w_full().h(1.0).fill(p.edge))
                .child(status::status_line(desk, &p)),
        );
    el().w_full()
        .h_full()
        .fill(p.bg)
        // Rune hands the root pastes nothing focused took (Edit ▸ Paste).
        .on_paste_content(|desk: &mut Desk, content, _| desk.paste_content(content))
        .child(main)
        .children(updates::card(desk, &p, band))
        .children(desk.inbox.open.then(|| inbox::view(desk, &p)))
        .children(desk.settings_open.then(|| settings::view(desk, &p)))
        .children(machines::sheet(desk, &p))
        .children(tabs::rename_sheet(desk, &p))
        .children(desk.palette_open.then(|| palette::view(desk, &p)))
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
                format!("{}-close", self.key),
                p,
                self.close,
            ));
        let panel = col()
            .key(self.key)
            .w(self.width)
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
            .items_center()
            .fill(Color::BLACK.alpha(self.scrim))
            .transition(Transition::opacity())
            .modal(true, self.close)
            .child(el().grow(1.0).max_h(self.top - MARGIN))
            .child(panel)
    }
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
