//! The playground views (galaxy, reef, pitch, kitchen, pit lane, hive) and
//! the way out every one of them shows. In the creature scenes a press on a
//! creature selects it; a press on the selected one (or Return) shows its
//! terminal, as the galaxy does. Escape lets go, then closes. Night follows
//! a dark theme.

use phux_protocol::ids::ResourceId;
use rune::{Cx, El, TextStyle, col, el, row, text};

use super::reef::rgba;
use super::{
    CAPTION, FROST, Ink, LABEL_STRONG, TITLEBAR_H, galaxy, hive, keycap, kitchen, overlay, pitch,
    pitlane, pressable, reef,
};
use crate::desk::Desk;
use crate::desk::playground::{Playground, Scene};
use crate::reef::{Mood, Pt, Rgb};
use crate::ui::Cover;

/// A playground view: whether it is open, how it draws, how it closes.
type View = (bool, fn(&Desk, &Ink, &mut Cx<Desk>) -> El, fn(&mut Desk));

/// The open playground view, if any, with the way out every one of them
/// shows. A new view is one more row here.
pub(super) fn open(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> Option<El> {
    let views: [View; 6] = [
        (desk.galaxy.open, galaxy::view, Desk::close_galaxy),
        (
            desk.reef.open,
            reef::view,
            Desk::close_scene::<crate::reef::Reef>,
        ),
        (
            desk.pitch.open,
            pitch::view,
            Desk::close_scene::<crate::pitch::Pitch>,
        ),
        (
            desk.kitchen.open,
            kitchen::view,
            Desk::close_scene::<crate::kitchen::Kitchen>,
        ),
        (
            desk.pitlane.open,
            pitlane::view,
            Desk::close_scene::<crate::pitlane::PitLane>,
        ),
        (
            desk.hive.open,
            hive::view,
            Desk::close_scene::<crate::hive::Hive>,
        ),
    ];
    let (_, view, close) = views.into_iter().find(|v| v.0)?;
    // A child of the view: the view is modal, so anything beside it is inert.
    let right = cx.chrome_insets().trailing;
    Some(view(desk, p, cx).child(esc_chip(p, close, right)))
}

/// A frosted key cap, top right in the titlebar band clear of every view's
/// own chrome: `esc` closes this. Escape lets go of a pick first; a press
/// here always closes.
fn esc_chip(p: &Ink, close: fn(&mut Desk), right: f32) -> El {
    pressable("playground-esc", p)
        .pin(None, Some(9.0), Some(16.0 + right), None)
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
        .child(keycap("esc", p))
        .child(text("close").font(CAPTION).color(p.dim).a11y_hidden())
}

/// The root a creature view draws in, labelled with its tally, and how many
/// of its agents need you. `title` names it to assistive technology.
pub(super) fn stage<S: Scene>(
    v: &Playground<S, S::Paint>,
    key: &'static str,
    title: &str,
    hosts: usize,
    p: &Ink,
    cx: &Cx<Desk>,
) -> (El, usize) {
    v.night.set(p.bg.luminance() < 0.5);
    v.still.set(cx.is_reduced_motion());
    let count = |m: Mood| v.scene.agents().filter(|(_, mood)| *mood == m).count();
    let waiting = count(Mood::Waiting);
    let summary = format!(
        "{title} with {} agents on {hosts} hosts: {} working, {waiting} need you, {} done, {} idle",
        v.scene.agents().count(),
        count(Mood::Working),
        count(Mood::Done),
        count(Mood::Idle),
    );
    let root = el()
        .key(key)
        .cover()
        .fill(p.bg)
        .modal(false, Desk::close_scene::<S>)
        .label(summary)
        .on_layout(|desk: &mut Desk, rect| desk.scene_laid_out::<S>(rect[2], rect[3]));
    (root, waiting)
}

/// Who a creature is, for its press target and its card.
pub(super) struct Who<'a> {
    pub id: &'a ResourceId,
    pub name: &'a str,
    pub mood: Mood,
    pub host: &'a str,
}

/// A creature's press target, a circle of `rad` round `at`: a press selects
/// it, a press on the selected one opens its terminal.
pub(super) fn target<S: Scene>(kind: &str, who: &Who, at: Pt, rad: f32) -> El {
    let (press, hover) = (who.id.clone(), who.id.clone());
    el().key(format!("{kind}-{:?}", who.id))
        .absolute(at[0] - rad, at[1] - rad)
        .size(2.0 * rad, 2.0 * rad)
        .radius(rad)
        .cursor(rune::Cursor::Pointer)
        .label(format!("{}, {}, {}", who.name, who.mood.word(), who.host))
        .on_press(move |desk: &mut Desk| desk.scene_press::<S>(press.clone()))
        .on_hover_change(move |desk: &mut Desk, on, _| desk.scene_hover::<S>(hover.clone(), on))
}

/// Along the foot of a scene `h` tall (under the titlebar until laid out):
/// the tally, the way to the next agent that needs you, the selected one's
/// card (`card` under its name, mood and machine) and the way out.
pub(super) fn readout<S: Scene>(
    key: &str,
    scene: &S,
    [hosts, waiting]: [usize; 2],
    h: f32,
    picked: Option<(Who, Option<String>)>,
    p: &Ink,
) -> El {
    let working = scene.agents().filter(|(_, m)| *m == Mood::Working).count();
    let picked = picked.map(|(who, card)| {
        let line = format!("{} · {} · {}  ↩", who.name, who.mood.word(), who.host);
        pressable(format!("{key}-open"), p)
            .px(12.0)
            .py(5.0)
            .radius(12.0)
            .fill(p.raised.alpha(0.85))
            .label(format!("Open {}", who.name))
            .on_press(Desk::open_agent::<S>)
            .child(
                col()
                    .gap(2.0)
                    .child(text(line).font(LABEL_STRONG).color(p.text))
                    .children(card.map(|c| text(c).font(CAPTION).color(p.dim))),
            )
    });
    let y = if h > 0.0 { h - 44.0 } else { TITLEBAR_H + 4.0 };
    row()
        .absolute(16.0, y)
        .gap(10.0)
        .items_center()
        .child(
            el().px(10.0)
                .py(4.0)
                .radius(10.0)
                .fill(p.raised.alpha(0.85))
                .child(
                    text(format!(
                        "{hosts} hosts · {} agents · {working} working",
                        scene.agents().count()
                    ))
                    .font(CAPTION)
                    .color(p.dim),
                ),
        )
        .children((waiting > 0).then(|| {
            pressable(format!("{key}-next-waiting"), p)
                .px(12.0)
                .py(5.0)
                .radius(12.0)
                .fill(p.marks.alert)
                .label("Show the next agent that needs you")
                .on_press(Desk::scene_next_waiting::<S>)
                .child(
                    text(format!("{waiting} need you"))
                        .font(LABEL_STRONG)
                        .color(crate::theme::on_fill(p.marks.alert)),
                )
        }))
        .children(picked)
        .child(overlay::icon_button(
            &super::icons::CLOSE,
            "Close",
            format!("{key}-close"),
            p,
            Desk::close_scene::<S>,
        ))
}

/// The amber of an agent that needs you, and what reads on it.
pub(super) const AMBER: Rgb = [255.0, 181.0, 71.0];
pub(super) const ON_AMBER: Rgb = [42.0, 22.0, 0.0];
const PILL_H: f32 = 19.0;

/// A name pill's text, and whether it is amber: an agent that needs you
/// says so.
pub(super) fn pill_label(name: &str, mood: Mood) -> (String, bool) {
    if mood == Mood::Waiting {
        (format!("{name} · needs you"), true)
    } else {
        (name.to_owned(), false)
    }
}

/// A name pill: amber when its agent needs you, else dark glass.
pub(super) fn name_pill(label: String, amber: bool, font: TextStyle) -> El {
    let (fill, ink) = if amber {
        (rgba(AMBER, 0.96), rgba(ON_AMBER, 1.0))
    } else {
        (
            rgba([10.0, 14.0, 20.0], 0.78),
            rgba([238.0, 242.0, 246.0], 1.0),
        )
    };
    row()
        .h(PILL_H)
        .radius(PILL_H / 2.0)
        .items_center()
        .fill(fill)
        .child(text(label).font(font).color(ink))
}

/// An amber count `d` across of agents needing you.
pub(super) fn badge(n: usize, d: f32, font: TextStyle) -> El {
    el().size(d, d)
        .radius(d / 2.0)
        .center()
        .fill(rgba(AMBER, 1.0))
        .child(text(n.to_string()).font(font).color(rgba(ON_AMBER, 1.0)))
}

/// Left edges and centre heights for pills of width `w` wanted centred on
/// `at`, the prototype's `pill()`: each kept in the view and nudged up past
/// those already placed, up to six times. Give them bottom-most first, so
/// stacks grow upward.
pub(super) fn place_pills(pills: &[(Pt, f32)], view_w: f32) -> Vec<Pt> {
    let mut placed: Vec<[f32; 3]> = Vec::new();
    for &(at, w) in pills {
        let x = (at[0] - w / 2.0).clamp(6.0, (view_w - w - 6.0).max(6.0));
        let mut y = at[1];
        for _ in 0..6 {
            let hit = placed.iter().any(|p| {
                (p[1] - y).abs() < PILL_H + 2.0 && x < p[0] + p[2] + 4.0 && p[0] < x + w + 4.0
            });
            if !hit {
                break;
            }
            y -= PILL_H + 3.0;
        }
        placed.push([x, y, w]);
    }
    placed.into_iter().map(|p| [p[0], p[1]]).collect()
}

/// A pill's estimated width for `label` at `per` points a character.
// ponytail: an estimate keeps pills apart and in view; measure the text if
// they ever clip or overlap.
pub(super) fn pill_w(label: &str, per: f32) -> f32 {
    label.chars().count() as f32 * per + 14.0
}

#[cfg(test)]
mod tests {
    #[test]
    fn close_pills_stack_upward_and_stay_in_view() {
        let spots = super::place_pills(
            &[
                ([100.0, 200.0], 80.0),
                ([110.0, 200.0], 80.0),
                ([400.0, 200.0], 80.0),
                ([2.0, 50.0], 80.0),
            ],
            500.0,
        );
        assert_eq!(spots[0], [60.0, 200.0]);
        assert_eq!(spots[1], [70.0, 178.0], "nudged up one pill");
        assert_eq!(spots[2], [360.0, 200.0], "clear of the others");
        assert_eq!(spots[3], [6.0, 50.0], "kept inside the view");
    }
}
