//! A session: its header above a grouped card of its panes, as an inset
//! grouped list draws a section. The header opens the session, its chevron
//! folds it, and a hold on either the header or a row opens their menu.
use crate::ui::tokens::Voice;
use phux_design::State;
use rune::{El, col, row, text};

use super::resource::Place;
use super::{Desk, Look, card, control, resource_row, separator};
use crate::addressable::Addressable;
use crate::desk::phone::navigation::Held;
use crate::ui::icons;
use crate::ui::tokens::Paint;

/// What a session is up to, read once from its members' marks.
pub(super) struct Tally {
    pub waiting: usize,
    pub live: usize,
}

impl Tally {
    pub fn of(desk: &Desk, members: &[&Addressable]) -> Self {
        let marks: Vec<_> = members
            .iter()
            .map(|r| desk.activities.mark(&r.id))
            .collect();
        Self {
            waiting: marks.iter().filter(|m| m.needs_you()).count(),
            live: marks.iter().filter(|m| m.live()).count(),
        }
    }

    /// Nothing moving and nothing asking: the session can fold away.
    pub const fn quiet(&self) -> bool {
        self.waiting == 0 && self.live == 0
    }
}

pub(super) fn session_section(
    desk: &Desk,
    session: &phux_client_runtime::control::SessionDescriptor,
    members: &[&Addressable],
    filtered: &[&Addressable],
    // None while searching: every match shows, and nothing offers to fold.
    fold: Option<bool>,
    p: &Look,
) -> El {
    let folded = fold == Some(true);
    let mut section = col()
        .key(format!("phone-session-section-{}", session.id))
        .w_full()
        .gap(p.space.s1)
        .child(session_header(desk, session, members, fold, p));
    if !folded && !filtered.is_empty() {
        section = section.child(rows(desk, filtered, Place::Session, p));
    }
    section
}

/// Panes as one grouped card, an inset hairline between rows.
pub(super) fn rows(desk: &Desk, resources: &[&Addressable], place: Place, p: &Look) -> El {
    let mut surface = card(p);
    for (i, resource) in resources.iter().enumerate() {
        if i > 0 {
            surface = surface.child(separator(p));
        }
        surface = surface.child(resource_row(desk, resource, place, p));
    }
    surface
}

fn session_header(
    desk: &Desk,
    session: &phux_client_runtime::control::SessionDescriptor,
    members: &[&Addressable],
    fold: Option<bool>,
    p: &Look,
) -> El {
    let folded = fold == Some(true);
    let s = &p.space;
    let dim = p.roles.text.dim.paint();
    let tally = Tally::of(desk, members);
    let panes = match members.len() {
        0 => "No panes".to_owned(),
        1 => "1 pane".to_owned(),
        n => format!("{n} panes"),
    };
    let name = session.name.clone();
    let id = session.id;
    let current = desk.selected_session_name().as_deref() == Some(&session.name);
    let attention =
        phux_design::plural("count.needs_you", tally.waiting as u64).unwrap_or_default();
    let label = format!(
        "{} · {panes}{} · {attention}{}",
        session.name,
        if current { " · current session" } else { "" },
        if folded { " · folded" } else { "" },
    );
    // One summary line: where you are, how big it is, what is moving.
    // Attention has its own mark and count, so it is not repeated here.
    let mut summary = Vec::new();
    if current {
        summary.push("Current".to_owned());
    }
    summary.push(panes);
    if tally.live > 0 {
        summary.push(format!("{} working", tally.live));
    }
    let open = control(&format!("phone-session-{id}"), &label, p, move |d, _| {
        d.phone_session(&name)
    })
    .grow(1.0)
    .min_w(0.0)
    .min_h(s.touch_min)
    .gap(s.s3)
    .px(s.s1)
    .on_secondary_press_local(move |d: &mut Desk, event, tx| {
        let at = [event.window_x, event.window_y];
        d.open_phone_menu(Held::Session(id), format!("phone-session-{id}"), at, tx);
    })
    .child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(s.s0_5)
            .child(
                text(session.name.clone())
                    .key(format!("phone-session-title-{id}"))
                    .font(p.ty.lead.semibold())
                    .color(p.roles.text.text.paint())
                    .truncate()
                    .a11y_hidden(),
            )
            // The summary carries "1 working": it wraps, never cut, at the
            // largest text sizes, as a pane row's detail does (audit row 25).
            .child(
                text(summary.join(" · "))
                    .font(p.ty.caption)
                    .color(if current {
                        p.roles.accent.accent_text.paint()
                    } else {
                        dim
                    })
                    .wrap()
                    .a11y_hidden(),
            ),
    )
    .children(
        crate::session_color::of(&session.name)
            .map(|rgb| crate::host_color::mark(rgb, s.s2 + s.s0_5, p.roles.layer.ground.paint())),
    )
    .children((tally.waiting > 0).then(|| attention_count(tally.waiting, p)));
    let fold = (fold.is_some() && !members.is_empty()).then(|| {
        fold_control(
            &format!("phone-session-fold-{id}"),
            if folded {
                format!("Show {}", session.name)
            } else {
                format!("Fold {}", session.name)
            },
            folded,
            p,
            move |d| d.toggle_phone_fold(id),
        )
    });
    row()
        .w_full()
        .items_center()
        .gap(s.s1)
        .child(open)
        .children(fold)
}

/// A chevron that folds what is under it: down when shown, right when
/// folded, as a disclosure does.
fn fold_control(
    key: &str,
    label: String,
    folded: bool,
    p: &Look,
    toggle: impl Fn(&mut Desk) + Clone + 'static,
) -> El {
    let size = p.icon.grid * p.type_scale;
    control(key, &label, p, move |d, _| toggle(d))
        .px(0.0)
        .center()
        .child(
            rune::icon(
                if folded {
                    &icons::CHEVRON_RIGHT
                } else {
                    &icons::CHEVRON
                },
                size,
            )
            .color(p.roles.text.dim.paint())
            .a11y_hidden(),
        )
}

/// Attention is a mark and number, not another surface. The header names it.
pub(super) fn attention_count(count: usize, p: &Look) -> El {
    let tone = p.status.state(State::NeedsYou).tone;
    let s = &p.space;
    row()
        .gap(s.s1)
        .min_h(p.icon.size.lg + s.s1)
        .items_center()
        .child(
            rune::icon(p.status.state(State::NeedsYou).icon, p.icon.size.xs)
                .color(tone.ink)
                .a11y_hidden(),
        )
        .child(
            text(count.to_string())
                .font(p.ty.caption.semibold().figures())
                .color(tone.ink),
        )
        .a11y_hidden()
}

/// A band's heading over its card: a quiet small label and a count, as an
/// inset grouped list titles its sections.
pub(super) fn band_heading(key: &str, title: &str, count: usize, p: &Look) -> El {
    let s = &p.space;
    let dim = p.roles.text.dim.paint();
    row()
        .key(key.to_owned())
        .w_full()
        .px(s.s1)
        .pt(s.s1)
        .items_center()
        .gap(s.s2)
        .role("heading")
        .label(format!("{title}, {count}"))
        .child(
            text(title.to_owned())
                .font(p.ty.caption.semibold())
                .color(dim)
                .grow(1.0)
                .min_w(0.0)
                .truncate()
                .a11y_hidden(),
        )
        .child(
            text(count.to_string())
                .font(p.ty.caption.figures())
                .color(dim)
                .a11y_hidden(),
        )
}
