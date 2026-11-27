//! A session header discloses its panes. A hold on the header opens session
//! actions, including Open; a hold on a pane opens its own menu.
use crate::ui::tokens::Voice;
use phux_design::State;
use rune::{El, col, row, text};

use super::resource::Place;
use super::{Desk, Look, card, control, grid, resource_row};
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
    card(
        resources
            .iter()
            .map(|resource| resource_row(desk, resource, place, p)),
        p,
    )
}

fn session_header(
    desk: &Desk,
    session: &phux_client_runtime::control::SessionDescriptor,
    members: &[&Addressable],
    fold: Option<bool>,
    p: &Look,
) -> El {
    let fold = fold.filter(|_| !members.is_empty());
    let s = &p.space;
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
    summary.push(panes);
    if tally.live > 0 {
        summary.push(format!("{} working", tally.live));
    }
    let color = crate::session_color::of(&session.name)
        .map(|rgb| crate::host_color::mark(rgb, p.icon.size.xs, p.roles.layer.ground.paint()));
    let title = heading_title(session.name.clone(), p).key(format!("phone-session-title-{id}"));
    let header = control(&format!("phone-session-{id}"), &label, p, move |d, _| {
        if fold.is_some() {
            d.toggle_phone_fold(id);
        } else {
            d.phone_session(&name);
        }
    })
    .on_secondary_press_local(move |d: &mut Desk, event, tx| {
        let at = [event.window_x, event.window_y];
        d.open_phone_menu(Held::Session(id), format!("phone-session-{id}"), at, tx);
    });
    let heading = heading_body(
        row()
            .w_full()
            .gap(s.s2)
            .items_center()
            .child(title.shrink())
            .children(color),
        Some(summary.join(" · ")),
        p,
    );
    // The ⋯ takes the trailing column the rows' checks share, so the
    // header's own padding ends at it.
    let header = with_count(heading_frame(header, p).pr(0.0), heading, tally.waiting, p);
    let header = match fold {
        Some(folded) => header.expanded(!folded).child(chevron(folded, p)),
        None => header.child(chevron(true, p)),
    };
    row()
        .key(format!("phone-session-header-{id}"))
        .w_full()
        .items_center()
        .pr(grid::clear_inset(p))
        .child(header.grow(1.0).min_w(0.0))
        .child(more(id, &session.name, p))
}

/// The session's menu in plain sight: the verbs a hold on its header
/// opens (Open, New Pane, Fold, Rename, Close Session), one tap away.
fn more(id: u32, name: &str, p: &Look) -> El {
    let key = format!("phone-session-more-{id}");
    let row = key.clone();
    control(&key, &format!("More for {name}"), p, move |d, tx| {
        d.open_phone_more(Held::Session(id), row.clone(), tx);
    })
    .px(0.0)
    .center()
    .radius(p.radius.full)
    .child(
        rune::icon(&icons::MORE, grid::trail_mark(p))
            .color(p.roles.text.dim.paint())
            .a11y_hidden(),
    )
}

/// Every section heading's frame: on the card's content inset, a touch row
/// tall, its trailing marks on the rows' trailing column.
pub(super) fn heading_row(heading: El, p: &Look) -> El {
    heading_frame(heading, p).w_full()
}

/// [`heading_row`] without its width, to share a row with a control.
fn heading_frame(heading: El, p: &Look) -> El {
    heading
        .min_h(p.space.touch_min)
        .px(grid::inset(p))
        .py(p.space.s1)
        .gap(p.space.s2)
        .items_center()
}

/// A section's name, as every section heading sets it.
/// One line at ordinary sizes, as a system list cuts a long name; whole,
/// wrapped, at the accessibility sizes, where one line holds a word.
pub(super) fn heading_title(title: String, p: &Look) -> El {
    let title = text(title)
        .font(p.ty.lead.semibold())
        .color(p.roles.text.text.paint())
        .min_w(0.0)
        .a11y_hidden();
    if p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale {
        title.wrap()
    } else {
        title.truncate()
    }
}

/// Whether a heading's attention count sits under its name rather than on
/// the trailing column: at the accessibility sizes, where a column beside
/// the name would break even a two-word name across lines.
fn count_under(p: &Look) -> bool {
    p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale
}

/// `heading` (a [`heading_body`]) in `frame`, with the attention count for
/// `waiting` panes, if any: on its own column, or under the name.
pub(super) fn with_count(frame: El, heading: El, waiting: usize, p: &Look) -> El {
    let count = (waiting > 0).then(|| attention_count(waiting, p));
    if count_under(p) {
        frame.child(heading.children(count))
    } else {
        frame.child(heading).children(count)
    }
}

/// A heading's name over its optional one-line summary in quiet ink; the
/// summary wraps, never cut, at the largest text sizes (audit row 25).
pub(super) fn heading_body(title: El, summary: Option<String>, p: &Look) -> El {
    col()
        .grow(1.0)
        .min_w(0.0)
        .gap(p.space.s0_5)
        .child(title)
        .children(summary.filter(|s| !s.is_empty()).map(|summary| {
            text(summary)
                .font(p.ty.caption)
                .color(p.roles.text.dim.paint())
                .wrap()
                .a11y_hidden()
        }))
}

/// A trailing disclosure on the same grid as the pane selection check.
pub(super) fn chevron(points_right: bool, p: &Look) -> El {
    let icon = if points_right {
        &icons::CHEVRON_RIGHT
    } else {
        &icons::CHEVRON
    };
    let mark = rune::icon(icon, grid::trail_mark(p)).color(p.roles.text.dim.paint());
    grid::trail_slot(Some(mark), grid::trail_w(p), p)
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

/// The Needs you band's heading, set as every section heading is: its
/// name, the count as the sessions' headings mark attention, and the
/// disclosure on their column, here to the Inbox, where every question
/// can be answered in place. A touch row tall, as theirs are. At the
/// accessibility sizes the name takes the whole measure, with the count
/// and the disclosure under it.
pub(super) fn inbox_heading(key: &str, title: &str, count: usize, label: String, p: &Look) -> El {
    let frame = heading_row(
        control(key, &label, p, |d: &mut Desk, tx| {
            d.phone_action(crate::keys::Shortcut::Inbox, tx);
        }),
        p,
    );
    let name = heading_body(
        heading_title(title.to_owned(), p).key(format!("{key}-title")),
        None,
        p,
    );
    let count = (count > 0).then(|| attention_count(count, p));
    if !count_under(p) {
        return frame.child(name).children(count).child(chevron(true, p));
    }
    let disclosure = rune::icon(&icons::CHEVRON_RIGHT, grid::trail_mark(p))
        .color(p.roles.text.dim.paint())
        .a11y_hidden();
    frame.child(
        name.child(
            row()
                .gap(p.space.s1)
                .items_center()
                .children(count)
                .child(disclosure),
        ),
    )
}
