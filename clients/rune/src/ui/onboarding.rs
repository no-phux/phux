//! First-run onboarding, drawn where the terminals go: the sidebar and title
//! bar stay, so every pick on the Look step shows on the real window. While
//! an agent brings a setup over, its pane sits beside the step.

use rune::{Align, Color, Cx, DragItem, El, TextStyle, Transition, col, el, row, text};

use super::{CAPTION, CARD_GAP, Ink, LABEL, LABEL_STRONG, icons, panes, pressable};
use crate::desk::Desk;
use crate::desk::onboarding::{Migrate, Probe};
use crate::keys::{self, Shortcut};
use crate::onboarding::{self as words, Grant, PERMISSIONS, Service, Step};
use crate::theme::{self, Theme};

const TITLE: TextStyle = TextStyle::new(26.0, 700.0);
const LEAD: TextStyle = TextStyle::new(13.5, 450.0).lh(19.0);
const COPY: TextStyle = TextStyle::new(12.0, 450.0).lh(16.0);
const CARD_W: f32 = 236.0;

mod agents;
mod done;
mod look;
mod migrate;
#[cfg(test)]
mod tests;

/// The terminal area: the step when onboarding is open, the panes otherwise,
/// and both while the migration's agent works.
pub fn stage(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    if !desk.onboarding.open || desk.bare.is_some() {
        return panes::view(desk, p, cx);
    }
    desk.onboarding.fox.still.set(cx.is_reduced_motion());
    let step = panes::card(
        el().w_full().h_full().fill(p.bg).child(page(desk, p)),
        p.bg,
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
        .gap(CARD_GAP)
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
fn page(desk: &Desk, p: &Ink) -> El {
    let step = desk.onboarding.step;
    let body = match step {
        Step::Welcome => welcome(desk, p),
        Step::Look => look::look(desk, p),
        Step::Agents => agents::agents(desk, p),
        Step::Migrate => migrate::migrate(desk, p),
        Step::Done => done::done(desk, p),
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
                .px(24.0)
                .py(28.0)
                .gap(14.0)
                .items_center()
                .transition(Transition::opacity())
                .child(body)
                .children(nav(desk, p)),
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
                .then(|| agents::grant_sheet(desk, p)),
        )
}

fn heading(glyph: &'static rune::Icon, title: &'static str, lead: &'static str, p: &Ink) -> El {
    col()
        .w_full()
        .max_w(560.0)
        .gap(8.0)
        .items_center()
        .child(tile(glyph, 64.0, p))
        .child(
            text(title)
                .font(TITLE)
                .color(p.text)
                .align(Align::Center)
                .role("heading")
                .level(1),
        )
        .child(
            text(lead)
                .font(LEAD)
                .color(p.dim)
                .align(Align::Center)
                .wrap(),
        )
}

/// An icon on a rounded accent tile.
fn tile(glyph: &'static rune::Icon, size: f32, p: &Ink) -> El {
    el().size(size, size)
        .radius(size * 0.25)
        .continuous_corners()
        .fill(p.accent.alpha(0.16))
        .border(1.0, p.accent.alpha(0.28))
        .center()
        .child(rune::icon(glyph, size * 0.5).color(p.accent))
        .a11y_hidden()
}

/// The accent pill: Continue, Install, Start.
fn primary(key: &'static str, label: &'static str, p: &Ink, press: fn(&mut Desk)) -> El {
    pressable(key, p)
        .h(34.0)
        .px(18.0)
        .gap(8.0)
        .radius(17.0)
        .items_center()
        .fill(p.accent)
        .focusable(true)
        .role("button")
        .label(label)
        .on_press(press)
        .child(
            text(label)
                .font(TextStyle::new(14.0, 600.0))
                .color(p.on_accent)
                .a11y_hidden(),
        )
        .child(
            rune::icon(&icons::ARROW_RIGHT, 14.0)
                .color(p.on_accent)
                .a11y_hidden(),
        )
}

/// A quiet pill.
fn quiet(key: impl Into<String>, label: impl Into<String>, p: &Ink) -> El {
    pill(key, label.into(), p.current, p.text, p)
}

fn pill(key: impl Into<String>, label: String, fill: Color, ink: Color, p: &Ink) -> El {
    pressable(key, p)
        .h(28.0)
        .px(12.0)
        .radius(14.0)
        .items_center()
        .fill(fill)
        .focusable(true)
        .role("button")
        .label(label.clone())
        .child(text(label).font(LABEL_STRONG).color(ink).a11y_hidden())
}

/// A rounded word on a tinted ground: Recommended, 1 of 3 granted.
fn badge(words: impl Into<String>, ink: Color) -> El {
    el().px(8.0)
        .py(2.0)
        .radius(9.0)
        .border(1.0, ink.alpha(0.5))
        .fill(ink.alpha(0.1))
        .child(text(words.into()).font(CAPTION).color(ink))
        .self_center()
}

/// A green check and a word: Installed, Granted.
fn good(word: &'static str, p: &Ink) -> El {
    row()
        .h(28.0)
        .px(12.0)
        .gap(6.0)
        .radius(14.0)
        .items_center()
        .fill(p.marks.good.alpha(0.12))
        .child(rune::icon(&icons::CHECK, 13.0).color(p.marks.good))
        .child(text(word).font(LABEL_STRONG).color(p.marks.good))
}

/// Back and Continue, under every step but the first and last.
fn nav(desk: &Desk, p: &Ink) -> Option<El> {
    let step = desk.onboarding.step;
    if matches!(step, Step::Welcome | Step::Done) {
        return None;
    }
    let back = pressable("onboarding-back", p)
        .size(34.0, 34.0)
        .radius(17.0)
        .center()
        .fill(p.accent.alpha(0.14))
        .focusable(true)
        .label("Back")
        .on_press(Desk::onboarding_back)
        .child(
            rune::icon(&icons::ARROW_LEFT, 14.0)
                .color(p.accent)
                .a11y_hidden(),
        );
    Some(
        row()
            .pt(10.0)
            .gap(10.0)
            .items_center()
            .child(back)
            .child(primary(
                "onboarding-continue",
                "Continue",
                p,
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

fn welcome(desk: &Desk, p: &Ink) -> El {
    col()
        .w_full()
        .max_w(520.0)
        .gap(12.0)
        .items_center()
        .child(art(desk.fox_image(), 136.0).a11y_hidden())
        .child(
            text("Get started with phux")
                .font(TextStyle::new(30.0, 700.0))
                .color(p.text)
                .align(Align::Center)
                .role("heading")
                .level(1),
        )
        .child(
            text(
                "Pick a look, give your agents the phux skill and the permissions they \
                 need, and bring your key bindings with you.",
            )
            .font(LEAD)
            .color(p.dim)
            .align(Align::Center)
            .wrap(),
        )
        .child(el().h(6.0))
        .child(primary(
            "onboarding-continue",
            "Continue",
            p,
            Desk::onboarding_continue,
        ))
        .child(
            pressable("onboarding-skip", p)
                .px(10.0)
                .py(4.0)
                .radius(6.0)
                .focusable(true)
                .label("Skip for now")
                .on_press(Desk::skip_onboarding)
                .child(
                    text("Skip for now")
                        .font(LABEL)
                        .color(p.accent)
                        .a11y_hidden(),
                ),
        )
}

fn panel(p: &Ink) -> El {
    col()
        .w_full()
        .max_w(600.0)
        .padding(16.0)
        .gap(16.0)
        .radius(14.0)
        .continuous_corners()
        .fill(p.raised.alpha(0.6))
        .border(1.0, p.separator)
}

fn note(words: impl Into<String>, p: &Ink) -> El {
    text(words.into())
        .font(COPY)
        .color(p.dim)
        .align(Align::Center)
        .wrap()
}

const UPDATE: &str = "Update phux to enable this.";
