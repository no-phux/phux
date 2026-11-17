//! First-run onboarding, drawn where the terminals go: the sidebar and title
//! bar stay, so every pick on the Look step shows on the real window. While
//! an agent brings a setup over, its pane sits beside the step.
//!
//! Actions are kit buttons (`Submit` for the step's one primary action),
//! choices kit checkboxes, status the OWN `tone_badge` (§4.1.3).

use phux_design::State;
use rune::{Align, Color, Cx, DragItem, El, TextStyle, Transition, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize};

use super::badge::tone_badge;
use super::settings::{grown, medium, semibold};
use super::tokens::{Paint, Tier, Tokens};
use super::{Ink, icons, panes};
use crate::desk::Desk;
use crate::desk::onboarding::{Migrate, Probe};
use crate::keys::{self, Shortcut};
use crate::onboarding::{self as words, Grant, PERMISSIONS, Service, Step};
use crate::theme::{self, Theme};

mod agents;
mod done;
// style-literals: illustration. The Look step's little windows are art.
#[allow(clippy::disallowed_methods)]
mod look;
mod machines;
mod migrate;
#[cfg(test)]
mod tests;

/// Onboarding's geometry: column widths and art sizes the token schema has
/// no role for. Component sizes, not style.
struct Geometry {
    /// The widest a step's heading and lead run.
    heading_max: f32,
    welcome_max: f32,
    /// A panel of choices, and the narrower single-column ones.
    panel_max: f32,
    list_max: f32,
    machines_max: f32,
    status_max: f32,
    sheet_max: f32,
    /// One of the Agents step's cards.
    card_w: f32,
    card_min_h: f32,
    /// The heading's icon tile, and a card's.
    hero_tile: f32,
    card_tile: f32,
    /// The brand fox on Welcome and Done, and the app badge to drag.
    fox: f32,
    fox_done: f32,
    badge: f32,
    /// A Bring-your-setup agent tile.
    agent_tile_w: f32,
    agent_tile_h: f32,
}

const GEOMETRY: Geometry = Geometry {
    heading_max: 560.0,
    welcome_max: 520.0,
    panel_max: 600.0,
    list_max: 400.0,
    machines_max: 460.0,
    status_max: 420.0,
    sheet_max: 480.0,
    card_w: 236.0,
    card_min_h: 220.0,
    hero_tile: 64.0,
    card_tile: 44.0,
    fox: 136.0,
    fox_done: 112.0,
    badge: 44.0,
    agent_tile_w: 84.0,
    agent_tile_h: 68.0,
};

/// The terminal area: the step when onboarding is open, the panes otherwise,
/// and both while the migration's agent works.
pub fn stage(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    // The grant sheet moves the window in device pixels.
    desk.scale.set(cx.env().scale);
    if !desk.onboarding.open || desk.bare.is_some() {
        return panes::view(desk, p, cx);
    }
    let t: &Tokens = p.as_ref();
    desk.onboarding.fox.still.set(cx.is_reduced_motion());
    let ground = t.roles.layer.ground.paint();
    let step = panes::card(
        el().w_full().h_full().fill(ground).child(page(desk, t)),
        ground,
        panes::Lit::Resting,
        p,
    );
    if !desk.onboarding.migrating() {
        return el()
            .key("onboarding")
            .grow(1.0)
            .w_full()
            .min_h(0.0)
            .child(step);
    }
    row()
        .key("onboarding")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .gap(t.space.card_gap)
        .child(el().w(0.0).grow(0.8).min_w(0.0).h_full().child(step))
        .child(
            col()
                .w(0.0)
                .grow(1.2)
                .min_w(0.0)
                .h_full()
                .child(panes::view(desk, p, cx)),
        )
}

/// One step, centred while it fits and scrolling when it doesn't.
fn page(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let step = desk.onboarding.step;
    let body = match step {
        Step::Welcome => welcome(desk, t),
        Step::Look => look::look(desk, t),
        Step::Agents => agents::agents(desk, t),
        Step::Migrate => migrate::migrate(desk, t),
        Step::Machines => machines::machines(desk, t),
        Step::Done => done::done(desk, t),
    };
    let content = col()
        .key(format!("onboarding-{}", step.id()))
        .w_full()
        .shrink()
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(
            col()
                .w_full()
                .px(s.s6)
                .py(s.s8)
                .gap(s.s4)
                .items_center()
                .transition(Transition::opacity())
                .child(body)
                .children(nav(desk, t)),
        );
    col()
        .w_full()
        .h_full()
        .role("dialog")
        .label("Get started with phux")
        .child(el().grow(1.0))
        .child(content)
        .child(el().grow(1.0))
        .children(
            desk.onboarding
                .grant_sheet
                .then(|| agents::grant_sheet(desk, t)),
        )
}

fn heading(glyph: &'static rune::Icon, title: &'static str, lead: &'static str, t: &Tokens) -> El {
    col()
        .w_full()
        .max_w(GEOMETRY.heading_max)
        .gap(t.space.s2)
        .items_center()
        .child(tile(glyph, GEOMETRY.hero_tile, t))
        .child(
            text(title)
                .font(t.ty.display)
                .color(t.roles.text.text.paint())
                .align(Align::Center)
                .role("heading")
                .level(1),
        )
        .child(
            text(lead)
                .font(t.ty.lead)
                .color(t.roles.text.dim.paint())
                .align(Align::Center)
                .wrap(),
        )
}

/// An icon on a neutral tile: colour means state, never decoration (D8).
fn tile(glyph: &'static rune::Icon, size: f32, t: &Tokens) -> El {
    let radius = if size > GEOMETRY.card_tile {
        t.radius.lg
    } else {
        t.radius.md
    };
    el().size(size, size)
        .radius(radius)
        .continuous_corners()
        .fill(t.roles.wash.current.paint())
        .border(t.space.hairline, t.roles.border.base.paint())
        .center()
        .child(rune::icon(glyph, size / 2.0).color(t.roles.text.text.paint()))
        .a11y_hidden()
}

/// The step's one primary action (kit `Submit`): Continue, Start.
fn primary(key: &'static str, label: &'static str, t: &Tokens, press: fn(&mut Desk)) -> El {
    Button::new(label)
        .variant(ButtonVariant::Submit)
        .control_size(ControlSize::ExtraLarge)
        .on_press(press)
        .build(&t.kit)
        .key(key)
        .focusable(true)
        .min_h(grown(t.ty.body, t.space.s4))
}

/// A secondary action (kit `Outline`).
fn quiet(key: impl Into<String>, label: impl Into<String>, t: &Tokens) -> El {
    action(key, label.into(), false, t)
}

/// A kit button: `Submit` when `accent`, else `Outline`.
fn action(key: impl Into<String>, label: String, accent: bool, t: &Tokens) -> El {
    let variant = if accent {
        ButtonVariant::Submit
    } else {
        ButtonVariant::Outline
    };
    Button::new(label)
        .variant(variant)
        .control_size(ControlSize::Large)
        .build(&t.kit)
        .key(key)
        .focusable(true)
        .min_h(grown(t.ty.body, t.space.s2))
}

/// A neutral word on a hairline: Recommended. Not a status, so no tone.
fn badge(words: impl Into<String>, t: &Tokens) -> El {
    el().px(t.space.s2)
        .py(t.space.s0_5)
        .radius(t.radius.xs)
        .border(t.space.hairline, t.roles.border.control.paint())
        .child(
            text(words.into())
                .font(t.ty.caption)
                .color(t.roles.text.dim.paint()),
        )
        .self_center()
}

/// Done, as colour, mark and word: Installed, Granted.
fn good(word: &'static str, t: &Tokens) -> El {
    tone_badge(t, State::Done, Some(word.to_owned()))
}

/// Back and Continue, under every step but the first and last.
fn nav(desk: &Desk, t: &Tokens) -> Option<El> {
    let step = desk.onboarding.step;
    if matches!(step, Step::Welcome | Step::Done) {
        return None;
    }
    let back = super::icon_press(
        &icons::ARROW_LEFT,
        "Back",
        "onboarding-back",
        t,
        Desk::onboarding_back,
    )
    .focusable(true);
    Some(
        row()
            .pt(t.space.s3)
            .gap(t.space.s3)
            .items_center()
            .child(back)
            .child(primary(
                "onboarding-continue",
                "Continue",
                t,
                Desk::onboarding_continue,
            )),
    )
}

/// Brand art at `size`: blank, at that size, until it decodes.
fn art(image: Option<&rune::Image>, size: f32) -> El {
    let frame = el().size(size, size);
    match image {
        Some(image) => frame.pixels(image.pixels()),
        None => frame,
    }
}

fn welcome(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    col()
        .w_full()
        .max_w(GEOMETRY.welcome_max)
        .gap(s.s3)
        .items_center()
        .child(art(desk.fox_image(), GEOMETRY.fox).a11y_hidden())
        .child(
            text("Get started with phux")
                .font(t.ty.display)
                .color(t.roles.text.text.paint())
                .align(Align::Center)
                .role("heading")
                .level(1),
        )
        .child(
            text(words::PROMISE.welcome)
                .font(t.ty.lead)
                .color(t.roles.text.dim.paint())
                .align(Align::Center)
                .wrap(),
        )
        .child(el().h(s.s2))
        .child(primary(
            "onboarding-continue",
            "Continue",
            t,
            Desk::onboarding_continue,
        ))
        .child(
            Button::new("Skip for now")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Large)
                .on_press(Desk::skip_onboarding)
                .build(&t.kit)
                .key("onboarding-skip")
                .focusable(true)
                .min_h(grown(t.ty.body, t.space.s2)),
        )
}

/// A group of choices: an opaque raised card (no ad hoc translucency,
/// §1.2), `radius.lg`.
fn panel(t: &Tokens) -> El {
    let s = &t.space;
    col()
        .w_full()
        .max_w(GEOMETRY.panel_max)
        .padding(s.s4)
        .gap(s.s4)
        .radius(t.radius.lg)
        .continuous_corners()
        .fill(t.roles.layer.raised.paint())
        .border(s.hairline, t.roles.border.base.paint())
}

fn note(words: impl Into<String>, t: &Tokens) -> El {
    text(words.into())
        .font(t.ty.body)
        .color(t.roles.text.dim.paint())
        .align(Align::Center)
        .wrap()
}

const UPDATE: &str = "Update phux to enable this.";
