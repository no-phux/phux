//! The playground views (galaxy, reef, pitch, kitchen, pit lane, hive) and
//! the way out every one of them shows.

use phux_protocol::ids::ResourceId;
use rune::{Cx, El, col, el, row, text};

use super::{
    CAPTION, FROST, Ink, LABEL_STRONG, TITLEBAR_H, galaxy, hive, keycap, kitchen, overlay, pitch,
    pitlane, pressable, reef,
};
use crate::desk::Desk;
use crate::desk::playground::{Playground, Scene};
use crate::reef::{Mood, Pt};

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
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
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
