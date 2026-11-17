//! One screen-addressable resource: identity, useful detail and its state
//! as the status ladder draws it (colour, mark and word together).
use rune::{El, col, el, text};

use super::{Desk, Look, control, medium, nested, row_h};
use crate::activity::Mark;
use crate::addressable::Addressable;
use crate::ui::icons;
use crate::ui::phone::{status_mark, word};
use crate::ui::tokens::Paint;

pub(super) fn resource_row(desk: &Desk, resource: &Addressable, p: &Look) -> El {
    let s = &p.space;
    let mark = desk.activities.mark(&resource.id);
    let current = desk
        .connection
        .shown
        .as_ref()
        .is_some_and(|s| s.resource == resource.id);
    let detail = resource_detail(desk, resource, mark);
    // Satellite rows have no session card above them. Keep their owning host
    // visible even when two machines have identically named panes.
    let detail = if resource.id.host().is_some() {
        [resource.session.as_str(), &detail]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    } else {
        detail
    };
    let (id, session) = (resource.id.clone(), resource.session.clone());
    let label = format!(
        "{} · {} · {detail}{}",
        resource.label,
        resource.session,
        if current { " · current pane" } else { "" }
    );
    // Faint never sits on a wash: the selected row's detail is dim.
    let detail_ink = p.roles.text.dim.paint();
    let row = control(
        &format!("phone-session-pane-{}", resource.id),
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
    .radius(nested(p))
    .focus_selected(current)
    .children([
        state_mark(mark, p),
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(s.s1)
            .child(
                text(resource.label.clone())
                    .font(medium(p.ty.label))
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
        el().size(p.icon.grid, p.icon.grid)
            .children(current.then(|| {
                rune::icon(&icons::CHECK, p.icon.grid)
                    .color(p.roles.accent.accent_text.paint())
                    .a11y_hidden()
            })),
    ]);
    if current {
        crate::ui::lifted(row, p)
    } else {
        row
    }
}

/// Session membership is already the group heading. Spend the second line on
/// the agent and its state, or the shell's folder, not repeated breadcrumbs.
fn resource_detail(desk: &Desk, resource: &Addressable, mark: Mark) -> String {
    let agent = desk.activities.agent_name(&resource.id).unwrap_or_default();
    let state = if mark == Mark::Idle { "" } else { word(mark) };
    let activity = [agent, state]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if !activity.is_empty() {
        return activity;
    }
    if !resource.kind.is_terminal() {
        return resource.mirror.to_owned();
    }
    desk.connection
        .topology
        .as_ref()
        .and_then(|t| t.pane(&resource.id))
        .and_then(|pane| pane.cwd.as_deref())
        .map(|cwd| {
            let trimmed = cwd.trim_end_matches('/');
            trimmed
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(cwd)
        })
        .unwrap_or_default()
        .to_owned()
}

/// The terminal's state mark, a plain shell's slot left empty so names
/// line up. Never animated: a busy agent costs no frames.
fn state_mark(mark: Mark, p: &Look) -> El {
    let size = p.icon.grid;
    el().size(size, size)
        .center()
        .children(status_mark(mark, p.icon.size.md, p))
        .a11y_hidden()
}
