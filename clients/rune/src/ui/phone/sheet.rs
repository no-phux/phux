//! The one chrome every phone sheet wears, whether it fills the page
//! (Terminal actions, the switcher, the Inbox, the palette) or rests on the
//! bottom edge (Machines, What's new, rename): a grabber at its top centre
//! that says it pulls down, then a header row of one geometry, its title on
//! the page margin and its round close on the trailing one. A bottom sheet
//! sits in the room above the key bar and keyboard, so its field and its
//! actions stay in reach of the thumb while it is typed into.
use rune::{El, Transition, col, el, row, text};

use super::motion;
use crate::desk::Desk;
use crate::desk::phone::dismiss::{Away, Bottom};
use crate::ui::Cover;
use crate::ui::tokens::phone::{page_margin, ty};
use crate::ui::tokens::{Paint, Tier, Tokens, Voice};

/// A sheet's grabber, out of flow at its top centre `top` points down (a
/// page under a desktop-hosted phone's title bar starts lower): it says
/// the sheet pulls down, and draws only.
pub(in crate::ui) fn grabber(top: f32, p: &Tokens) -> El {
    let s = &p.space;
    el().key("phone-sheet-grabber")
        .pin(Some(0.0), Some(top + s.s1 + s.s0_5), Some(0.0), None)
        .center()
        .child(
            el().w(s.s8 + s.s1)
                .h(s.s1 + s.s0_5 / 2.0)
                .radius(p.radius.full)
                .fill(p.roles.border.control.paint()),
        )
        .inert()
        .a11y_hidden()
}

/// A sheet's title in the phone's own type: the heading its header leads
/// with, beside its close.
pub(in crate::ui) fn title(words: &'static str, p: &Tokens) -> El {
    text(words)
        .font(title_type(p))
        .color(p.roles.text.text.paint())
        .role("heading")
        .wrap()
        .min_w(0.0)
}

/// The type of a sheet's title. It grows with the requested text size up
/// to the grid's cap, as iOS stops a bar's title, so at the largest sizes
/// it keeps beside its close and leaves the room above the keyboard to
/// what the sheet holds.
pub(in crate::ui) fn title_type(p: &Tokens) -> rune::TextStyle {
    let scale = p.type_scale.min(crate::desk::accessibility::GRID_CAP);
    ty(scale).title.semibold()
}

/// A sheet's header: `title` on the page margin and `trailing` (its round
/// close last) ending on the other, one touch row tall, clear of the
/// grabber above it.
pub(in crate::ui) fn header(title: El, trailing: impl IntoIterator<Item = El>, p: &Tokens) -> El {
    let s = &p.space;
    row()
        .w_full()
        .min_h(s.touch_min + s.s2)
        .pt(s.s2)
        .px(page_margin())
        .gap(s.s2)
        .items_center()
        .child(title.grow(1.0).shrink())
        .children(trailing)
}

/// What a bottom sheet holds, beside which one it is.
pub(in crate::ui) struct BottomSheet {
    pub(in crate::ui) which: Bottom,
    pub(in crate::ui) title: &'static str,
    pub(in crate::ui) close: fn(&mut Desk),
    /// The sheet's content; it scrolls when the room above the keyboard
    /// cannot hold it, its header and `foot` always showing.
    pub(in crate::ui) body: El,
    pub(in crate::ui) foot: Option<El>,
}

/// A sheet resting on the bottom edge of the room above the key bar: a
/// raised card with rounded top corners under its grabber and header,
/// rising whole on the drawer spring over the scrim every modal sits on,
/// and lowered by a swipe down as by its close. It is keyed by `which`;
/// `{key}-close` keys its close and `{key}-body` its scroller.
pub(in crate::ui) fn bottom(desk: &Desk, sheet: BottomSheet, p: &Tokens) -> El {
    let s = &p.space;
    let r = &p.roles;
    let key = sheet.which.key();
    let radius = p.radius.xl;
    let away = Away::Bottom(sheet.which);
    let close =
        super::close_button(desk, &format!("{key}-close"), "Close", p).on_press(sheet.close);
    let body = col()
        .key(sheet.which.body())
        .w_full()
        .shrink()
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(s.s6)
        .scroll_margin(s.s6)
        .child(sheet.body);
    let panel = col()
        .key(key)
        .w_full()
        .shrink()
        .min_h(0.0)
        .pt(s.s2)
        .pb(s.s3)
        .corners([radius, radius, 0.0, 0.0])
        .clip_corners([radius, radius, 0.0, 0.0])
        .continuous_corners()
        .fill(r.layer.raised.paint())
        .transition(motion::sheet(p))
        .child(grabber(0.0, p))
        .child(header(title(sheet.title, p), [close], p))
        .child(super::swipe_list(desk, body, away))
        .children(
            sheet
                .foot
                .map(|foot| el().w_full().px(page_margin()).pt(s.s2).child(foot)),
        );
    let panel = super::swipeable(desk, panel, away, p);
    let stack = place(col())
        // The room above stays the scrim's, a tap there closing the sheet.
        .child(el().grow(1.0).min_h(s.touch_min))
        .child(p.elevate(panel, Tier::Overlay));
    holder(&format!("{key}-scrim"), sheet.title, sheet.close, true, p).child(stack)
}

/// What a phone sheet sits in: the modal that traps focus and closes on
/// Escape or a press beside the sheet, and, with `dim`, the scrim that
/// fades in under it. The holder itself neither fades nor moves, so the
/// sheet it holds rises whole and opaque rather than fading in as one
/// layer with it. It leaves lowering and fading, the sheet in it.
///
/// Rune plays an entering element's transition only where its parent's
/// did not play, so whatever stands between the holder and the sheet is
/// [`place`]d: it enters with no effect of its own, leaving the rise to
/// the sheet.
pub(in crate::ui) fn holder(
    key: &str,
    label: &str,
    close: fn(&mut Desk),
    dim: bool,
    p: &Tokens,
) -> El {
    let scrim = el()
        .key(format!("{key}-dim"))
        .cover()
        .fill(p.roles.layer.scrim.paint())
        .transition(Transition::opacity().springs(p.motion.drawer))
        .inert()
        .a11y_hidden();
    el().key(key)
        .cover()
        .clip_rounded(0.0)
        .label(label.to_owned())
        // It arrives as nothing, leaving the entrance to the sheet; the
        // sheet goes as part of it, so it lowers as the sheet would.
        .transition(motion::holding(motion::sheet(p)))
        .modal(true, close)
        .children(dim.then_some(scrim))
}

/// `e`, filling the holder, as a container that enters with no effect of
/// its own: the sheet inside it plays its rise.
pub(in crate::ui) fn place(e: El) -> El {
    e.w_full().h_full().transition(Transition::none())
}
