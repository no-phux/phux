//! The chrome's shared controls under their long-standing names: sheets,
//! presses, keycaps, tooltips. Views call these; Lane A re-implements them
//! on tokens.

use rune::{Color, El, TextStyle, Transition, col, el, row, text};

use super::{CAPTION, Ink, LABEL_STRONG, icons, overlay};
use crate::desk::Desk;

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

#[allow(clippy::disallowed_methods)] // converted in the values commit
impl Sheet {
    /// `body` scrolls when the window can't hold it; the header and `foot`
    /// always show, down to the smallest window.
    pub(crate) fn build(self, aside: Option<El>, body: El, foot: El, p: &Ink) -> El {
        const MARGIN: f32 = 16.0;
        const MAX_H: f32 = 620.0;
        let line = || rune::rule(1.0, p.separator).w_full();
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
                &icons::CLOSE,
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
            .child(line().key("sheet-header-rule"))
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
            .child(line().key("sheet-footer-rule"))
            .child(el().w_full().px(14.0).py(10.0).child(foot));
        col()
            .key(format!("{}-scrim", self.key))
            .cover()
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
        .size(26.0, 26.0)
        .radius(7.0)
        .center()
        .label(name)
        .on_press(press)
        .child(
            rune::icon(glyph, 16.0)
                .color(p.dim)
                .group_color(p.text)
                .a11y_hidden(),
        )
        .hover_group()
}

/// Covering the whole of its parent, over what came before it.
pub(crate) trait Cover {
    fn cover(self) -> Self;
}

impl Cover for El {
    fn cover(self) -> El {
        self.absolute(0.0, 0.0).w_full().h_full()
    }
}

/// Something to press: a wash under the pointer that deepens while held, and
/// the pointer. Key errors name the caller, not this.
#[track_caller]
pub(crate) fn pressable(key: impl Into<String>, p: &Ink) -> El {
    el().key(key)
        .hover(p.hover)
        .press(p.hover)
        .cursor(rune::Cursor::Pointer)
}

/// Where you are in the chrome: a row or tab lifted a shade off its ground,
/// lit from the top as the terminal cards are.
#[allow(clippy::disallowed_methods)] // converted in the values commit
pub(crate) fn lifted(el: El, p: &Ink) -> El {
    el.fill(p.current)
        .border_gradient(1.0, p.text.alpha(0.14), p.text.alpha(0.03))
}

/// A key as the chrome draws one, wherever it names a chord.
#[allow(clippy::disallowed_methods)] // converted in the values commit
pub(crate) fn keycap(key: impl Into<String>, p: &Ink) -> El {
    const KEY: TextStyle = TextStyle::new(10.5, 600.0);
    el().h(16.0)
        .min_w(16.0)
        .px(4.0)
        .radius(4.0)
        .center()
        .fill(p.text.alpha(0.07))
        .border_gradient(1.0, p.text.alpha(0.2), p.text.alpha(0.1))
        .child(text(key).font(KEY).color(p.dim).a11y_hidden())
}

/// A tooltip for `el`: what pressing it does, and the chord that does the
/// same from the keyboard.
pub(crate) fn tip(el: El, words: impl Into<String>, action: crate::keys::Shortcut) -> El {
    let keys = crate::keys::command(action).map_or_else(Vec::new, |command| command.keycaps());
    el.help(words).help_keys(keys)
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
