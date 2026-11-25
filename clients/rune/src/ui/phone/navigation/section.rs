//! A session anchor and its indented panes, on the page's continuous ground.
use crate::ui::tokens::Voice;
use phux_design::State;
use rune::{El, col, row, text};

use super::{Desk, Look, control, resource_row};
use crate::addressable::Addressable;
use crate::ui::tokens::Paint;

pub(super) fn session_section(
    desk: &Desk,
    session: &phux_client_runtime::control::SessionDescriptor,
    members: &[&Addressable],
    filtered: &[&Addressable],
    p: &Look,
) -> El {
    let s = &p.space;
    let dim = p.roles.text.dim.paint();
    let marks: Vec<_> = members
        .iter()
        .map(|r| desk.activities.mark(&r.id))
        .collect();
    let waiting = marks.iter().filter(|m| m.needs_you()).count();
    let live = marks.iter().filter(|m| m.live()).count();
    let panes = match members.len() {
        0 => "No panes".to_owned(),
        1 => "1 pane".to_owned(),
        n => format!("{n} panes"),
    };
    let name = session.name.clone();
    let current = desk.selected_session_name().as_deref() == Some(&session.name);
    let attention = phux_design::plural("count.needs_you", waiting as u64).unwrap_or_default();
    let label = format!(
        "{} · {panes}{} · {attention}",
        session.name,
        if current { " · current session" } else { "" },
    );
    // One summary line: where you are, how big it is, what is moving.
    // Attention has its own mark and count, so it is not repeated here.
    let mut summary = Vec::new();
    if current {
        summary.push("Current".to_owned());
    }
    summary.push(panes.clone());
    if live > 0 {
        summary.push(format!("{live} working"));
    }
    let header = control(
        &format!("phone-session-{}", session.id),
        &label,
        p,
        move |d, _| d.phone_session(&name),
    )
    .w_full()
    .min_h(s.touch_min)
    .gap(s.s3)
    .px(0.0)
    .child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(s.s0_5)
            .child(
                text(session.name.clone())
                    .key(format!("phone-session-title-{}", session.id))
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
    .children((waiting > 0).then(|| attention_count(waiting, p)));
    let mut section = col()
        .key(format!("phone-session-section-{}", session.id))
        .w_full()
        .child(header);
    for resource in filtered {
        section = section.child(resource_row(desk, resource, p));
    }
    section
}

/// Attention is a mark and number, not another surface. The header names it.
fn attention_count(count: usize, p: &Look) -> El {
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
