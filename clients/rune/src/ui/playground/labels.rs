//! Labels shared by world-space playground scenes: name tags over the
//! scene and the count of agents that need you, on the window's tokens.
//! A tag floats over illustration, so it is always opaque: the overlay
//! layer, or needs-you's wash already composited over it (`flat`).

use phux_design::{Layer, State, Tone};
use rune::{Color, El, el, row, text};

use super::super::badge;
use super::super::tokens::{Paint, Tokens};
use crate::playground::Pt;

/// A pill over the scene, at a world point.
pub(crate) struct Label {
    pub at: Pt,
    pub text: String,
    /// The count of agents needing you.
    pub badge: Option<usize>,
    /// The tag of something that needs you: its tone, mark and word.
    pub needs_you: bool,
    /// A machine whose colour leads the pill.
    pub host: Option<String>,
}

/// A tag's height: a large icon and a hairline above and below it.
pub(crate) fn tag_h(t: &Tokens) -> f32 {
    t.icon.size.lg + 2.0 * t.space.hairline
}

/// A tag's fill and ink: the overlay layer, or needs-you composited over it.
pub(crate) fn tag_colors(t: &Tokens, needs_you: bool) -> (Color, Color) {
    let flat = t.roles.flat(Layer::Overlay);
    if needs_you {
        (
            flat.status.needs_you_wash.paint(),
            t.status.tone(Tone::NeedsYou).ink,
        )
    } else {
        (t.roles.layer.overlay.paint(), t.roles.text.text.paint())
    }
}

/// The empty tag a scene positions and fills: radius, fill and edge.
pub(crate) fn tag(t: &Tokens, needs_you: bool) -> El {
    let flat = t.roles.flat(Layer::Overlay);
    let edge = if needs_you {
        flat.status.needs_you_edge
    } else {
        flat.border.base
    };
    row()
        .h(tag_h(t))
        .px(t.space.s2)
        .gap(t.space.s1)
        .radius(t.radius.full)
        .items_center()
        .fill(tag_colors(t, needs_you).0)
        .border(t.space.hairline, edge.paint())
}

/// A name tag: needs-you's mark leads the words of one that needs you.
pub(crate) fn name_tag(t: &Tokens, words: String, needs_you: bool) -> El {
    let ink = tag_colors(t, needs_you).1;
    tag(t, needs_you)
        .children(needs_you.then(|| badge::mark(t, State::NeedsYou, t.icon.size.xs).a11y_hidden()))
        .child(text(words).font(t.ty.caption).color(ink))
}

/// How wide a tag of `chars` characters is taken to be when placing it:
/// a generous caption advance, the padding and, for needs-you, the mark.
pub(crate) fn tag_w(t: &Tokens, chars: usize, needs_you: bool) -> f32 {
    let mark = if needs_you {
        t.icon.size.xs + t.space.s1
    } else {
        0.0
    };
    chars as f32 * 0.6 * t.ty.caption.size + 2.0 * t.space.s2 + mark
}

/// The count of agents that need you: needs-you's ink as the fill, with
/// whichever of white or black reads on it.
pub(crate) fn count_colors(t: &Tokens) -> (Color, Color) {
    let fill = t.roles.status.needs_you;
    let lut = phux_design::Params::house().srgb_lut;
    let ink = phux_design::solve::on_fill(fill.rgb(), lut);
    (fill.paint(), ink.paint())
}

/// A round count of agents that need you, `d` across.
pub(crate) fn count(t: &Tokens, n: usize, d: f32) -> El {
    let (fill, ink) = count_colors(t);
    el().size(d, d)
        .radius(t.radius.full)
        .center()
        .fill(fill)
        .label(phux_design::plural("count.needs_you", n as u64).unwrap_or_default())
        .child(
            text(n.to_string())
                .font(t.ty.micro)
                .color(ink)
                .a11y_hidden(),
        )
}

/// A label pill centred on `at`, kept inside the view.
pub(crate) fn pill(t: &Tokens, l: &Label, at: Pt, w: f32) -> El {
    // ponytail: an estimated width keeps a pill inside the view; measure the
    // text if pills ever clip at the edges.
    let est = tag_w(t, l.text.chars().count(), l.needs_you)
        + l.badge.map_or(0.0, |_| t.icon.size.md + t.space.s1);
    let edge = t.space.s2;
    let x = if w > est + 2.0 * edge {
        at[0].clamp(edge + est / 2.0, w - edge - est / 2.0)
    } else {
        at[0]
    };
    let (fill, ink) = tag_colors(t, l.needs_you);
    let host = (l.host.as_deref()).map(|host| crate::host_color::dot(host, t.icon.size.xs, fill));
    // A centred track the tag sits in, wider than any scene label.
    let track = 2.0 * t.space.sidebar_w;
    row()
        .absolute(x - track / 2.0, at[1] - tag_h(t) / 2.0)
        .w(track)
        .justify_center()
        .child(
            tag(t, l.needs_you)
                .children(host)
                .children(
                    l.needs_you
                        .then(|| badge::mark(t, State::NeedsYou, t.icon.size.xs).a11y_hidden()),
                )
                .child(text(l.text.clone()).font(t.ty.caption).color(ink))
                .children(l.badge.map(|n| count(t, n, t.icon.size.md))),
        )
}
