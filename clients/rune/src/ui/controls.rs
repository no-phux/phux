//! The chrome's shared controls under their long-standing names: sheets,
//! presses, keycaps, tooltips, buttons. Each takes anything that holds the
//! window's [`Tokens`] and is built only from tokens and the kit.

use rune::{El, Transition, col, el, row, text};
use rune_kit::{IconButton, IconButtonVariant, Keybind};

use super::tokens::{Paint, SHEET_MAX_H, Tier, Tokens};
use crate::desk::Desk;

/// A modal sheet (OWN): scrim, header with a close button,
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
    /// always show, down to the smallest window. A phone has no keys for a
    /// foot to teach, so it may have none.
    pub(crate) fn build(self, aside: Option<El>, body: El, foot: Option<El>, p: &Tokens) -> El {
        let t = p;
        let s = &t.space;
        let line = || rune::rule(s.hairline, t.roles.border.base.paint()).w_full();
        let header = sheet_header(self.title, self.key, aside, self.close, p)
            .h(s.row_h + s.s4)
            .pl(s.sheet_pad)
            .pr(s.s2);
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
            // The one modal surface: raised, edged in `border.base`.
            .fill(t.roles.layer.raised.paint())
            .border(s.hairline, t.roles.border.base.paint())
            .transition(t.enter())
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
                    // What scrolls under the footer fades: there is more.
                    .scroll_edge_fade(s.s6)
                    .child(body),
            )
            .children(foot.map(|foot| {
                col()
                    .w_full()
                    .child(line().key("sheet-footer-rule"))
                    .child(el().w_full().px(s.card_inset).py(s.s2).child(foot))
            }));
        let panel = t.elevate(panel, Tier::Overlay);
        scrim(
            col()
                .key(format!("{}-scrim", self.key))
                .py(s.s4)
                .px(s.s4)
                .items_center(),
            self.title,
            Some(self.close),
            t,
        )
        .child(el().grow(1.0).max_h(self.top - s.s4))
        .child(panel)
    }
}

/// A modal sheet's header: its title (a heading) and close button, with
/// `aside` between them. `{key}-close` keys the button.
pub(crate) fn sheet_header(
    title: &'static str,
    key: &str,
    aside: Option<El>,
    close: fn(&mut Desk),
    t: &Tokens,
) -> El {
    row()
        .w_full()
        .gap(t.space.s2)
        .items_center()
        .child(
            text(title)
                .font(t.ty.title)
                .color(t.roles.text.text.paint())
                .role("heading")
                .level(2),
        )
        .child(el().grow(1.0))
        .children(aside)
        .child(icon_press(
            &super::icons::CLOSE,
            "Close",
            format!("{key}-close"),
            t,
            close,
        ))
}

/// The one scrim every modal sits on (`layer.scrim`): `base` over the
/// whole window, fading in and out, trapping focus as a modal named
/// `label`. `close` is what Escape and a press on the scrim do; `None`
/// leaves Escape to the content (the popup's command owns it).
pub(crate) fn scrim(base: El, label: &str, close: Option<fn(&mut Desk)>, t: &Tokens) -> El {
    let base = base
        .cover()
        .fill(t.roles.layer.scrim.paint())
        .transition(Transition::opacity())
        .label(label.to_owned());
    match close {
        Some(close) => base.modal(true, close),
        None => base.modal(false, |_: &mut Desk| {}),
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
/// its place while held, and the pointer. Key errors name the
/// caller, not this.
#[track_caller]
pub(crate) fn pressable(key: impl Into<String>, p: &Tokens) -> El {
    p.kit
        .interactive(el().key(key))
        .cursor(rune::Cursor::Pointer)
}

/// Shown at rest and hidden while the pointer is on the nearest hover
/// group, or (`on_hover`) the other way round: one of two things in the
/// same place. The chrome's one sanctioned opacity outside the owner
/// paths (DEVELOP.md, "Design tokens"): Rune's `group_opacity` swaps them
/// on the pointer without rebuilding the view, which a token colour swap
/// would need. It is a reveal of 0 or 1, never a dimmed look.
pub(crate) fn reveal(e: El, on_hover: bool) -> El {
    let (rest, hover) = if on_hover { (0.0, 1.0) } else { (1.0, 0.0) };
    e.group_opacity(rest, hover)
}

/// The selected row or tab: the one treatment.
pub(crate) use super::rows::lifted;

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

/// A secondary action in a row of them: kit `Button`, outlined, with the
/// kit's press scale on `spring.press`, in the Tab order.
pub(crate) fn button(
    label: &'static str,
    key: &'static str,
    p: &Tokens,
    press: fn(&mut Desk),
) -> El {
    super::settings::kit_button(key, label, rune_kit::ButtonVariant::Outline, p)
        .grow(1.0)
        .on_press(press)
}

/// `on_drag` for a surface floating over panes. Taking the press stops it,
/// and the wheel with it, reaching the pane underneath, without calling the
/// surface a button as `on_press` would.
pub(crate) fn keep_from_pane(_: &mut Desk, _: &mut rune::Drag, _: &mut rune::Tx) {}
