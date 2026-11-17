//! The chrome every playground view shows over its scene, on the window's
//! tokens: the readout along the foot (the fleet's counts, the next agent
//! that needs you, the picked agent's card and the close button) and the
//! `esc` chip in the titlebar band. Each chip floats over illustration, so
//! it is opaque: the overlay layer at the floating tier (§1.7, §1.9's
//! fallback), never a scene colour.

use phux_design::{State, Tone};
use rune::{El, TextStyle, col, el, row, text};

use super::super::badge;
use super::super::tokens::{Paint, Tier, Tokens};
use super::super::{icon_press, icons, keycap, pressable};
use super::labels;
use crate::desk::Desk;
use crate::playground::Mood;

/// What a view's readout says.
pub(crate) struct Readout {
    /// The view's key prefix: `{key}-next-waiting`, `{key}-open`, `{key}-close`.
    pub key: &'static str,
    /// The view's height once laid out (0 before).
    pub h: f32,
    pub hosts: usize,
    pub agents: usize,
    pub working: usize,
    pub waiting: usize,
    /// Shows the next agent that needs you.
    pub next: fn(&mut Desk),
    pub close: fn(&mut Desk),
    pub picked: Option<Picked>,
}

/// The picked agent's card: a press shows its terminal.
pub(crate) struct Picked {
    pub name: String,
    pub mood: Mood,
    pub host: String,
    /// Its rank or form and work, when its machine publishes agent stats.
    pub card: Option<String>,
    pub open: fn(&mut Desk),
}

/// A font's role at its semibold weight.
fn strong(role: TextStyle) -> TextStyle {
    let semibold = phux_design::tokens::TYPE.weights.semibold;
    TextStyle {
        weight: f32::from(u16::try_from(semibold).unwrap_or(u16::MAX)),
        ..role
    }
}

/// An opaque chip floating over the scene.
fn chip(t: &Tokens, e: El) -> El {
    let e = e
        .fill(t.roles.layer.overlay.paint())
        .border(t.space.hairline, t.roles.border.base.paint());
    t.elevate(e, Tier::Floating)
}

/// The readout along the view's foot: once laid out, a row's height above
/// it; before, just under the titlebar.
pub(crate) fn readout(t: &Tokens, r: Readout) -> El {
    let s = t.space;
    let y = if r.h > 0.0 {
        r.h - s.s10 - s.s1
    } else {
        s.titlebar_h + s.s1
    };
    let stats = chip(
        t,
        el().px(s.s3)
            .py(s.s1)
            .min_h(s.pointer_min)
            .items_center()
            .radius(t.radius.full),
    )
    .child(
        text(format!(
            "{} · {} · {} {}",
            plural(r.hosts, "host", "hosts"),
            plural(r.agents, "agent", "agents"),
            r.working,
            State::Working.word().unwrap_or_default(),
        ))
        .font(t.ty.caption)
        .color(t.roles.text.dim.paint()),
    );
    row()
        .absolute(s.s4, y)
        .gap(s.s2)
        .items_center()
        .child(stats)
        .children((r.waiting > 0).then(|| next_waiting(t, r.key, r.waiting, r.next)))
        .children(r.picked.map(|p| picked(t, r.key, p)))
        .child(close(t, r.key, r.close))
}

/// `n` and its noun: the readout's own counts, which the vocabulary has no
/// plural for.
fn plural(n: usize, one: &str, other: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { other })
}

/// Shows the next agent that needs you: needs-you's ink as the fill, its
/// mark and the vocabulary's plural ("1 needs you", "3 need you").
fn next_waiting(t: &Tokens, key: &str, waiting: usize, next: fn(&mut Desk)) -> El {
    let s = t.space;
    let (fill, ink) = labels::count_colors(t);
    let words = phux_design::plural("count.needs_you", waiting as u64)
        .unwrap_or_else(|| format!("{waiting}"));
    pressable(format!("{key}-next-waiting"), t)
        .px(s.s3)
        .py(s.s1)
        .min_h(s.pointer_min)
        .gap(s.s1)
        .items_center()
        .radius(t.radius.full)
        .fill(fill)
        .label("Show the next agent that needs you")
        .on_press(next)
        .child(
            rune::icon(t.status.state(State::NeedsYou).icon, t.icon.size.sm)
                .color(ink)
                .a11y_hidden(),
        )
        .child(text(words).font(strong(t.ty.label)).color(ink))
}

/// The picked agent: its mark, name, word and machine, then its card; a
/// press (or Return) shows its terminal.
fn picked(t: &Tokens, key: &str, p: Picked) -> El {
    let s = t.space;
    let line = format!("{} · {} · {}  ↩", p.name, p.mood.word(), p.host);
    chip(
        t,
        pressable(format!("{key}-open"), t)
            .px(s.s3)
            .py(s.s1)
            .gap(s.s2)
            .items_center()
            .radius(t.radius.lg),
    )
    .label(format!("Open {}", p.name))
    .on_press(p.open)
    .child(badge::mark(t, p.mood.state(), t.icon.size.sm).a11y_hidden())
    .child(
        col()
            .gap(s.s0_5)
            .child(
                text(line)
                    .font(strong(t.ty.label))
                    .color(t.roles.text.text.paint()),
            )
            .children(
                p.card
                    .map(|c| text(c).font(t.ty.caption).color(t.roles.text.dim.paint())),
            ),
    )
}

/// The view's close button: the one icon button, on a chip so it reads
/// over any scene.
pub(crate) fn close(t: &Tokens, key: &str, close: fn(&mut Desk)) -> El {
    chip(t, el().radius(t.radius.full).center()).child(icon_press(
        &icons::CLOSE,
        "Close",
        format!("{key}-close"),
        t,
        close,
    ))
}

/// A key cap, top right in the titlebar band clear of every view's own
/// chrome: `esc` closes this. Escape lets go of a pick first; a press here
/// always closes.
pub(crate) fn esc_chip(t: &Tokens, close: fn(&mut Desk), right: f32) -> El {
    let s = t.space;
    let top = (s.titlebar_h - s.pointer_min) / 2.0;
    chip(
        t,
        pressable("playground-esc", t)
            .pin(None, Some(top), Some(s.s4 + right), None)
            .min_h(s.pointer_min)
            .pl(s.s1)
            .pr(s.s2)
            .gap(s.s1)
            .items_center()
            .radius(t.radius.sm)
            .continuous_corners(),
    )
    .label("Close, Escape")
    .on_press(close)
    .child(keycap("esc", t))
    .child(
        text("close")
            .font(t.ty.caption)
            .color(t.roles.text.dim.paint())
            .a11y_hidden(),
    )
}

/// Needs-you's ink: what a scene's own chrome (the galaxy's command line)
/// draws an agent that needs you in.
pub(crate) fn needs_you_ink(t: &Tokens) -> rune::Color {
    t.status.tone(Tone::NeedsYou).ink
}
