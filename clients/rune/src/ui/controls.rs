//! The chrome's shared controls under their long-standing names: sheets,
//! presses, keycaps, tooltips, buttons. Each takes anything that holds the
//! window's [`Tokens`] and is built only from tokens and the kit.

use rune::{El, Transition, col, el, row, text};
use rune_kit::{IconButton, IconButtonVariant, Keybind};

use super::tokens::{Paint, SHEET_MAX_H, Tier, Tokens};
use crate::desk::Desk;

/// A modal sheet (§4.1.3 OWN): scrim, header with a close button,
/// scrolling body, footer. Kit `Dialog` cannot hold its top offset, footer
/// rule or scrolling body, so phux keeps its own on tokens, with Rune's
/// `.modal` for the focus trap and its return.
pub(crate) struct Sheet {
    pub(crate) key: &'static str,
    pub(crate) title: &'static str,
    pub(crate) width: f32,
    /// Where the sheet sits when the window has room for that.
    pub(crate) top: f32,
    pub(crate) close: fn(&mut Desk),
}

impl Sheet {
    /// `body` scrolls when the window can't hold it; the header and `foot`
    /// always show, down to the smallest window.
    pub(crate) fn build(self, aside: Option<El>, body: El, foot: El, p: &Tokens) -> El {
        let t = p;
        let s = &t.space;
        let line = || rune::rule(s.hairline, t.roles.border.base.paint()).w_full();
        let header = row()
            .w_full()
            .h(s.row_h + s.s4)
            .pl(s.sheet_pad)
            .pr(s.s2)
            .gap(s.s2)
            .items_center()
            .child(
                text(self.title)
                    .font(t.ty.title)
                    .color(t.roles.text.text.paint()),
            )
            .child(el().grow(1.0))
            .children(aside)
            .child(icon_press(
                &super::icons::CLOSE,
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
            .max_h(SHEET_MAX_H)
            .shrink()
            .min_h(0.0)
            .radius(t.radius.lg)
            .clip_rounded(t.radius.lg)
            .fill(t.roles.layer.raised.paint())
            // Exits fade; entries scale up from `enter_scale` (§1.8).
            .transition(Transition::asymmetric(
                Transition::scale(t.motion.enter_scale).springs(t.motion.pop),
                Transition::opacity(),
            ))
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
            .child(el().w_full().px(s.card_inset).py(s.s2).child(foot));
        let panel = t.elevate(panel, Tier::Overlay);
        col()
            .key(format!("{}-scrim", self.key))
            .cover()
            .py(s.s4)
            .px(s.s4)
            .items_center()
            .fill(t.roles.layer.scrim.paint())
            .transition(Transition::opacity())
            .modal(true, self.close)
            .label(self.title)
            .child(el().grow(1.0).max_h(self.top - s.s4))
            .child(panel)
    }
}

/// A quiet square button around an icon (kit `IconButton`, ghost); `name`
/// is what a screen reader says for it.
pub(crate) fn icon_press(
    glyph: &'static rune::Icon,
    name: &'static str,
    key: impl Into<String>,
    p: &Tokens,
    press: impl Fn(&mut Desk) + 'static,
) -> El {
    IconButton::new(glyph)
        .variant(IconButtonVariant::Ghost)
        .label(name)
        .on_press(press)
        .build(&p.kit)
        .key(key)
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

/// Something to press: the hover wash under the pointer, the press wash in
/// its place while held (§1.10), and the pointer. Key errors name the
/// caller, not this.
#[track_caller]
pub(crate) fn pressable(key: impl Into<String>, p: &Tokens) -> El {
    p.kit
        .interactive(el().key(key))
        .cursor(rune::Cursor::Pointer)
}

/// The selected row or tab: §1.10's one treatment.
pub(crate) fn lifted(el: El, p: &Tokens) -> El {
    super::rows::lifted(el, p)
}

/// A key as the chrome draws one, wherever it names a chord (kit `Keybind`).
pub(crate) fn keycap(key: impl Into<String>, p: &Tokens) -> El {
    let key: String = key.into();
    Keybind::new(&[key]).build(&p.kit).a11y_hidden()
}

/// A tooltip for `el`: what pressing it does, and the chord that does the
/// same from the keyboard.
pub(crate) fn tip(el: El, words: impl Into<String>, action: crate::keys::Shortcut) -> El {
    let keys = crate::keys::command(action).map_or_else(Vec::new, |command| command.keycaps());
    el.help(words).help_keys(keys)
}

/// A secondary action in a row of them: the control outline, label type.
pub(crate) fn button(
    label: &'static str,
    key: &'static str,
    p: &Tokens,
    press: fn(&mut Desk),
) -> El {
    let t = p;
    pressable(key, p)
        .grow(1.0)
        .min_h(t.space.pointer_min)
        .py(t.space.s1)
        .radius(t.radius.sm)
        .center()
        .border(t.space.hairline, t.roles.border.control.paint())
        .on_press(press)
        .child(
            text(label)
                .font(t.ty.caption)
                .color(t.roles.text.text.paint()),
        )
}

/// `on_drag` for a surface floating over panes. Taking the press stops it,
/// and the wheel with it, reaching the pane underneath, without calling the
/// surface a button as `on_press` would.
pub(crate) fn keep_from_pane(_: &mut Desk, _: &mut rune::Drag, _: &mut rune::Tx) {}
