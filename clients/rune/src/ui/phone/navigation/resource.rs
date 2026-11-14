//! One screen-addressable resource: identity, useful detail and a quiet state.
use rune::{El, col, el, text};

use super::{CAP_WORD, Desk, INSET, Ink, ROW_HEIGHT, SUBTITLE, control};
use crate::activity::Mark;
use crate::addressable::Addressable;
use crate::ui::{icons, sidebar};

pub(super) fn resource_row(desk: &Desk, resource: &Addressable, p: &Ink) -> El {
    let mark = desk.activities.mark(&resource.id);
    let current = desk
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
    control(
        &format!("phone-session-pane-{}", resource.id),
        &label,
        p,
        move |d, _| {
            d.close_phone_navigation();
            d.show(id.clone(), &session);
        },
    )
    .w_full()
    .min_h(ROW_HEIGHT)
    .gap(8.0)
    .focus_selected(current)
    .fill(if current {
        p.current
    } else {
        rune::Color::TRANSPARENT
    })
    .children([
        state_mark(mark, p),
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(INSET)
            .child(
                text(resource.label.clone())
                    .font(CAP_WORD)
                    .color(p.text)
                    .truncate()
                    .a11y_hidden(),
            )
            .children((!detail.is_empty()).then(|| {
                text(detail)
                    .font(SUBTITLE)
                    .color(p.dim)
                    .truncate()
                    .a11y_hidden()
            })),
        el().size(16.0, 16.0).children(current.then(|| {
            rune::icon(&icons::CHECK, 16.0)
                .color(p.accent)
                .a11y_hidden()
        })),
    ])
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
    if !resource.kind.is_terminal() {
        return resource.mirror.to_owned();
    }
    desk.topology
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

/// Quiet work is neutral, live work is a ring, attention is a solid themed
/// signal. No idle greens or ambient animation competing with what needs you.
fn state_mark(mark: Mark, p: &Ink) -> El {
    let dot = if mark.needs_you() {
        el().size(8.0, 8.0)
            .radius(INSET)
            .fill(sidebar::mark_color(mark, p))
    } else if mark.live() || mark == Mark::Background {
        el().size(8.0, 8.0).radius(INSET).border(1.5, p.dim)
    } else {
        el().size(INSET, INSET).radius(INSET / 2.0).fill(p.dim)
    };
    el().size(16.0, 16.0).center().child(dot).a11y_hidden()
}
