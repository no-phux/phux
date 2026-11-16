//! Labels shared by world-space playground scenes.

use rune::{El, el, row, text};

use super::super::CAPTION;
use super::pen::rgba;
use crate::playground::Pt;

/// A pill over the scene, at a world point.
pub(crate) struct Label {
    pub at: Pt,
    pub text: String,
    /// The count of agents needing you.
    pub badge: Option<usize>,
    pub amber: bool,
    /// A machine whose colour leads the pill.
    pub host: Option<String>,
}

const AMBER: [f32; 3] = [255.0, 181.0, 71.0];
const ON_AMBER: [f32; 3] = [42.0, 22.0, 0.0];

/// A label pill centred on `at`, kept inside the view.
pub(crate) fn pill(l: &Label, at: Pt, w: f32) -> El {
    // ponytail: an estimated width keeps a pill inside the view; measure the
    // text if pills ever clip at the edges.
    let est =
        l.text.chars().count() as f32 * 6.6 + 14.0 + if l.badge.is_some() { 18.0 } else { 0.0 };
    let x = if w > est + 12.0 {
        at[0].clamp(6.0 + est / 2.0, w - 6.0 - est / 2.0)
    } else {
        at[0]
    };
    let (fill, ink) = if l.amber {
        (rgba(AMBER, 0.95), rgba(ON_AMBER, 1.0))
    } else {
        (
            rgba([4.0, 20.0, 34.0], 0.72),
            rgba([228.0, 245.0, 247.0], 1.0),
        )
    };
    let host = (l.host.as_deref()).map(|host| crate::host_color::dot(host, 7.0, fill.alpha(1.0)));
    row()
        .absolute(x - 160.0, at[1] - 9.5)
        .w(320.0)
        .justify_center()
        .child(
            row()
                .h(19.0)
                .px(7.0)
                .gap(5.0)
                .radius(9.5)
                .items_center()
                .fill(fill)
                .children(host)
                .child(text(l.text.clone()).font(CAPTION).color(ink))
                .children(l.badge.map(|n| {
                    el().size(14.0, 14.0)
                        .radius(7.0)
                        .center()
                        .fill(rgba(AMBER, 1.0))
                        .child(text(n.to_string()).font(CAPTION).color(rgba(ON_AMBER, 1.0)))
                })),
        )
}
