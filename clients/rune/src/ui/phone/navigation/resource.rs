//! One screen-addressable resource: identity, useful detail and its state
//! as the status ladder draws it (colour, mark and word together).
use rune::{El, col, text};

use super::{Desk, Look, control, grid};
use crate::activity::Mark;
use crate::addressable::Addressable;
use crate::desk::phone::navigation::Held;
use crate::ui::icons;
use crate::ui::tokens::Paint;

/// Where a pane row is listed. Outside its session's card (Needs you,
/// another machine) the detail names its session, and a row listed twice
/// keeps a key of its own in each place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    Session,
    NeedsYou,
    Elsewhere,
}

impl Place {
    const fn key(self) -> &'static str {
        match self {
            Self::NeedsYou => "phone-needs-you-pane",
            Self::Session | Self::Elsewhere => "phone-session-pane",
        }
    }

    /// The current pane's check keeps the key its marker always had.
    const fn current_key(self) -> &'static str {
        match self {
            Self::NeedsYou => "phone-needs-you",
            Self::Session | Self::Elsewhere => "phone-session",
        }
    }
}

pub(super) fn resource_row(desk: &Desk, resource: &Addressable, place: Place, p: &Look) -> El {
    let s = &p.space;
    let mark = desk.activities.mark(&resource.id);
    let presentation = resource.presentation(desk);
    let current = desk
        .connection
        .shown
        .as_ref()
        .is_some_and(|s| s.resource == resource.id);
    let detail = resource_detail(desk, resource, mark);
    // Satellite and band rows have no session heading above them. Keep their
    // owning session visible even when two have identically named panes.
    let detail = if place != Place::Session || resource.id.host().is_some() {
        [resource.session.as_str(), &detail]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    } else {
        detail
    };
    let (id, session) = (resource.id.clone(), resource.session.clone());
    let held = Held::Pane(id.clone(), session.clone());
    let row_key = format!("{}-{}", place.key(), resource.id);
    let label = format!(
        "{} · {} · {detail}{}",
        presentation.name,
        resource.session,
        if current { " · current pane" } else { "" }
    );
    let line = p.ty.body.line_px();
    // Location has the one native cue: an accent check at the trailing
    // edge, a shape as well as a colour. No resting slab, no bar.
    let check = current.then(|| {
        rune::icon(&icons::CHECK, p.icon.size.md)
            .key(format!("{}-current-{}", place.current_key(), resource.id))
            .color(p.roles.accent.accent_text.paint())
            .inert()
    });
    let identity = crate::ui::tile::mark(
        &presentation.identity,
        mark,
        grid::card_fill(p),
        grid::glyph(p),
        p,
    );
    control(&row_key, &label, p, move |d, _| {
        d.close_phone_navigation();
        d.show(id.clone(), &session);
    })
    .w_full()
    .gap(s.s2)
    .px(grid::inset(p))
    .py(s.s2 + s.s0_5)
    .focus_selected(current)
    .on_secondary_press_local(move |d: &mut Desk, event, tx| {
        let at = [event.window_x, event.window_y];
        d.open_phone_menu(held.clone(), row_key.clone(), at, tx);
    })
    // The mark stays on the title's line when the detail wraps under it.
    .items_start()
    .child(grid::lead_slot(identity, line, p).key(format!("{}-mark-{}", place.key(), resource.id)))
    .child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(s.s0_5)
            .child(
                text(presentation.name)
                    .key(format!("{}-title-{}", place.key(), resource.id))
                    .font(p.ty.body)
                    .color(p.roles.text.text.paint())
                    .truncate()
                    .a11y_hidden(),
            )
            // The detail carries the status word: it wraps, never cut, at
            // the largest text sizes (audit row 25).
            .children((!detail.is_empty()).then(|| {
                text(detail)
                    .font(p.ty.caption)
                    .color(p.roles.text.dim.paint())
                    .wrap()
                    .a11y_hidden()
            })),
    )
    .child(grid::trail_slot(check, line, p))
}

/// Session membership is already the group heading. Spend the second line on
/// the agent and its state, or the shell's folder, not repeated breadcrumbs.
fn resource_detail(desk: &Desk, resource: &Addressable, mark: Mark) -> String {
    let agent = desk.activities.agent_name(&resource.id).unwrap_or_default();
    let state = if mark == Mark::Idle { "" } else { mark.word() };
    let activity = [agent, state]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if !activity.is_empty() {
        return activity;
    }
    resource.presentation(desk).detail
}
