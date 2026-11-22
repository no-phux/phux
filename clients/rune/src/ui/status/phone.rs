//! The phone's side of the window's words: what the phone app says where a
//! desktop would name a command, the way back to pairing, and where a banner
//! goes under a phone's top bar.

use phux_design::State;
use phux_design::tokens::SPACE;
use rune::{El, col, el, text};

use super::super::phone::top_bar_height;
use super::super::tokens::layout::STATUS;
use super::super::tokens::phone::ty;
use super::super::{Paint, icons, pressable};
use super::{Act, Tokens, Voice, server_behind};
use crate::desk::Desk;
use crate::desk::link::Standing;

/// A held confirmation on a phone, with the gap under it.
fn confirmation() -> f32 {
    SPACE.s16 + SPACE.s1
}

/// A desktop banner's top: under the title bar, and under a held
/// confirmation's bar when there is one.
fn desktop_top(held: bool) -> f32 {
    let top = SPACE.titlebar_h + SPACE.s1 + SPACE.s0_5;
    if held {
        top + SPACE.s12 + SPACE.s1
    } else {
        top
    }
}

/// The phone app's title and detail for `standing`, where they differ from
/// a desktop's: its computer by name, nothing to run in a shell.
pub(super) fn words(
    standing: &Standing,
    machine: &str,
    unpaired: bool,
) -> Option<(String, String)> {
    Some(match standing {
        Standing::Unreachable(why) if unpaired => ("No longer paired".to_owned(), why.clone()),
        Standing::Unreachable(why) => (format!("Can't reach {machine}"), why.clone()),
        Standing::Connecting(why) => (format!("Connecting to {machine}…"), why.clone()),
        Standing::Incompatible(words) if server_behind(words) => (
            format!("Update phux on {machine}"),
            format!(
                "This app is newer than phux on {machine}. Update phux there, then try again. \
                 {words}"
            ),
        ),
        _ => return None,
    })
}

/// Whether pairing again is what `standing` needs: the phone app's computer
/// refused its credential, which no retry changes.
pub(super) fn repairs(desk: &Desk, standing: &Standing) -> bool {
    desk.native.is_some() && desk.unpaired() && matches!(standing, Standing::Unreachable(_))
}

/// Whether the phone app's dial has outlasted a healthy one: `standing`
/// then carries what to check, and trying again is offered beside it.
pub(super) fn slow_dial(desk: &Desk, standing: &Standing) -> bool {
    desk.native.is_some() && matches!(standing, Standing::Connecting(why) if !why.is_empty())
}

/// Back to the host's pairing, which is where the phone's computers are.
pub(super) const fn pair_again(key: &'static str) -> Act {
    Act::new("Pair again", key, Desk::toggle_machines)
}

/// What the stage says in place of a terminal.
pub(super) struct Said<'a> {
    pub(super) standing: &'a Standing,
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) failed: bool,
}

/// The symbol over the words: what the wait or the failure is about.
fn glyph(standing: &Standing) -> &'static rune::Icon {
    match standing {
        Standing::Connecting(_) | Standing::Closed(_) | Standing::Unreachable(_) => {
            &icons::MACHINES
        }
        Standing::Incompatible(_) | Standing::NoCli(_) | Standing::Installing => &icons::BOLT,
        Standing::Starting
        | Standing::NoSessions
        | Standing::NoTerminals
        | Standing::Ended(_)
        | Standing::Opening => &icons::PROMPT,
    }
}

/// A round tile holding a symbol: a quiet raised disc, hidden from the
/// reader since the heading beside it already says what it means.
fn disc(key: &str, icon: &'static rune::Icon, fill: rune::Color, t: &Tokens) -> El {
    round(key, t)
        .fill(fill)
        .border(t.space.hairline, t.roles.border.muted.paint())
        .child(
            rune::icon(icon, t.icon.size.xl)
                .color(t.roles.text.dim.paint())
                .a11y_hidden(),
        )
        .a11y_hidden()
}

fn round(key: &str, t: &Tokens) -> El {
    let side = t.space.s16;
    el().key(format!("{key}-symbol"))
        .size(side, side)
        .radius(t.radius.full)
        .center()
}

/// The stage's symbol. A failure wears the failed tone's wash, edge and
/// mark, named by its word (§1.3: never colour alone).
fn tile(said: &Said, t: &Tokens) -> El {
    if !said.failed {
        // On the stage's ground, the disc rises a tier.
        let fill = t.roles.layer.raised.paint();
        return disc("placeholder", glyph(said.standing), fill, t);
    }
    let ink = t.status.state(State::Failed);
    round("placeholder", t)
        .fill(ink.tone.wash)
        .border(t.space.hairline, ink.tone.edge)
        .child(super::badge::mark(t, State::Failed, t.icon.size.xl))
}

/// A full-width capsule a thumb can find: the remedy filled in the accent,
/// a way elsewhere on the raised tier.
fn capsule(act: Act, t: &Tokens) -> El {
    let r = &t.roles;
    let words = ty(t.type_scale).body.semibold();
    let (fill, ink, edge) = if act.primary {
        (r.accent.accent.paint(), r.accent.on_accent.paint(), None)
    } else {
        (
            r.layer.raised.paint(),
            r.text.text.paint(),
            Some(r.border.base.paint()),
        )
    };
    let shell = pressable(act.key, t)
        .role("button")
        .label(act.label)
        .focusable(true)
        .w_full()
        .min_h(t.space.touch_min + t.space.s1)
        .px(t.space.s4)
        .radius(t.radius.full)
        .center()
        .fill(fill);
    let shell = match edge {
        Some(edge) => shell.border(t.space.hairline, edge),
        None => shell,
    };
    let press = act.press;
    shell.on_press(press).child(
        text(act.label)
            .font(words)
            .color(ink)
            .align(rune::Align::Center)
            .a11y_hidden(),
    )
}

/// The phone's stage with no terminal on it: a symbol, a heading and its
/// detail on one centred measure, then the remedy and a way elsewhere as
/// full-width touch targets. Still: nothing here animates while idle.
pub(super) fn stage(said: &Said, acts: &[Act], extra: Option<El>, t: &Tokens) -> El {
    let s = &t.space;
    let buttons = (!acts.is_empty()).then(|| {
        col()
            .key("placeholder-actions")
            .w_full()
            .gap(s.s2)
            .pt(s.s5)
            // One filled button leads; any further remedy is quieter.
            .children(acts.iter().enumerate().map(|(at, act)| {
                let leads = act.primary && !acts[..at].iter().any(|a| a.primary);
                capsule(Act { primary: leads, ..*act }, t)
            }))
    });
    message("placeholder", tile(said, t), &said.title, &said.detail, t)
        .children(extra.map(|e| el().pt(s.s4).child(e)))
        .children(buttons)
}

/// A phone's empty list on the raised tier: its symbol, what is missing
/// and what lands here, centred on the stage's measure.
pub(in crate::ui) fn empty(
    key: &str,
    icon: &'static rune::Icon,
    title: &str,
    detail: &str,
    t: &Tokens,
) -> El {
    // A list already sits on the raised tier: its disc sinks to the
    // sidebar's, so it still reads as a shape in a light theme.
    let fill = t.roles.layer.sidebar.paint();
    message(key, disc(key, icon, fill, t), title, detail, t)
}

/// A symbol over a heading and its detail, centred on one measure.
fn message(key: &str, symbol: El, title: &str, detail: &str, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let ty = ty(t.type_scale);
    let heading = text(title.to_owned())
        .key(format!("{key}-title"))
        .font(ty.title.semibold())
        .color(r.text.text.paint())
        .align(rune::Align::Center)
        .wrap()
        .w_full()
        .role("heading");
    let detail = (!detail.is_empty()).then(|| {
        text(detail.to_owned())
            .key(format!("{key}-detail"))
            .font(ty.body)
            .color(r.text.dim.paint())
            .align(rune::Align::Center)
            .wrap()
            .w_full()
    });
    col()
        .key(key.to_owned())
        .w_full()
        .max_w(STATUS.phone_measure + 2.0 * s.s6)
        .px(s.s6)
        .items_center()
        .child(symbol)
        .child(el().h(s.s4))
        .child(heading)
        .children(detail.map(|d| el().w_full().pt(s.s2).child(d)))
}

/// Where the banner floats: under a desktop's title bar or a phone's top
/// bar, and below a held confirmation, which keeps the top of the stage.
pub(super) fn banner_top(desk: &Desk) -> f32 {
    let held = desk.pending.is_some();
    if desk.phone.is_none() {
        return desktop_top(held);
    }
    // UIKit keeps the native surface inside the safe area; a desktop's
    // preview of the phone starts under its own title bar.
    let inset = if desk.native.is_some() {
        0.0
    } else {
        SPACE.titlebar_h
    };
    inset + top_bar_height() + SPACE.s1 + if held { confirmation() } else { 0.0 }
}
