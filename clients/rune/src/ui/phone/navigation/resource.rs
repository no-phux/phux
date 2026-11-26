//! One screen-addressable resource: identity, useful detail and its state
//! as the status ladder draws it (colour, mark and word together).
use rune::{El, col, el, text};

use super::{Desk, Look, control, nested, row_h};
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

    /// The current-pane bar keeps its old key in a session's card.
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
    let detail_ink = p.roles.text.dim.paint();
    let row = control(
        &row_key,
        &label,
        p,
        move |d, _| {
            d.close_phone_navigation();
            d.show(id.clone(), &session);
        },
    )
    .w_full()
    .min_h(row_h(p))
    .gap(s.s2)
    .pl(s.s3)
    .radius(nested(p))
    .focus_selected(current)
    .on_secondary_press_local(move |d: &mut Desk, event, tx| {
        let at = [event.window_x, event.window_y];
        d.open_phone_menu(held.clone(), row_key.clone(), at, tx);
    })
    // The mark stays on the title's line when the detail wraps under it.
    .items_start()
    .py(s.s2)
    .children([
        el().h(p.ty.body.line_px())
            .center()
            .child(crate::ui::tile::tile(&presentation.identity, mark, 1, p.roles.layer.raised.paint(), p)),
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(s.s1)
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
                    .color(detail_ink)
                    .wrap()
                    .a11y_hidden()
            })),
        // The check grows with the row's text, as the search magnifier does.
        el().size(p.icon.grid * p.type_scale, p.icon.grid * p.type_scale)
            .children(current.then(|| {
                rune::icon(&icons::CHECK, p.icon.grid * p.type_scale)
                    .color(p.roles.accent.accent_text.paint())
                    .a11y_hidden()
            })),
    ]);
    // Location has its own narrow cue and check, separate from agent state.
    // Keep the same structural cue in Increased Contrast; no resting slab.
    row.children(current.then(|| {
        crate::host_color::stripe(p.roles.accent.accent.paint(), p).key(format!(
            "{}-current-{}",
            place.current_key(),
            resource.id
        ))
    }))
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
