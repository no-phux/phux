//! Terminal actions: grouped touch lists with an icon per row.
use crate::ui::tokens::Voice;
use phux_design::State;
use rune::{El, col, el, text};

use super::{Desk, Look, card, control, grid};
use crate::keys::{self, Shortcut};
use crate::ui::icons;
use crate::ui::tokens::Paint;

/// Terminal actions as grouped lists: making, finding, naming, going
/// elsewhere, and closing on its own at the end in the failure tone.
pub(super) fn actions(desk: &Desk, p: &Look) -> El {
    /// The Inbox, Computers and Settings group, which What's new ends.
    const NEWS_GROUP: usize = 3;
    const GROUPS: [&[(Shortcut, &rune::Icon)]; 5] = [
        &[
            (Shortcut::NewTerminal, &icons::PLUS),
            (Shortcut::NewSession, &icons::WINDOW),
        ],
        &[
            (Shortcut::Find, &icons::SEARCH),
            (Shortcut::SplitRight, &icons::SPLIT_RIGHT),
            (Shortcut::SplitDown, &icons::SPLIT_DOWN),
        ],
        &[
            (Shortcut::RenameTab, &icons::PENCIL),
            (Shortcut::RenameSession, &icons::PENCIL),
        ],
        &[
            (Shortcut::Inbox, &icons::INBOX),
            (Shortcut::Machines, &icons::MACHINES),
            (Shortcut::Settings, &icons::SETTINGS),
        ],
        &[(Shortcut::CloseTerminal, &icons::CLOSE)],
    ];
    let s = &p.space;
    let mut list = col()
        .w_full()
        .px(grid::margin())
        .pt(s.s2)
        .pb(s.s6)
        .gap(grid::block_gap(p));
    for (index, group) in GROUPS.iter().enumerate() {
        let mut rows: Vec<El> = group
            .iter()
            .map(|&(action, icon)| action_row(desk, action, icon, p))
            .collect();
        // What's new joins the places the app keeps: the top bar has no
        // bell of its own, and ⋯ carries its unread dot.
        if index == NEWS_GROUP {
            rows.push(news_row(desk, p));
        }
        list = list.child(card(rows, p));
    }
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(s.s4)
        .child(list)
}

/// An action's icon in the rows' leading column.
fn lead_icon(icon: &'static rune::Icon, ink: rune::Color, p: &Look) -> El {
    grid::lead_slot(
        rune::icon(icon, grid::glyph(p)).color(ink),
        grid::glyph(p),
        p,
    )
    .a11y_hidden()
}

/// What's new, with the unread dot ⋯ carried on the terminal.
fn news_row(desk: &Desk, p: &Look) -> El {
    let unread = desk.news.unread;
    let s = &p.space;
    control(
        "phone-action-news",
        crate::ui::news::bell_name(desk),
        p,
        |d: &mut Desk, tx| {
            d.close_phone_navigation();
            tx.blur();
            if !d.news.open {
                d.toggle_news();
            }
        },
    )
    .w_full()
    .gap(s.s2)
    .px(grid::inset(p))
    .child(lead_icon(&icons::BELL, p.roles.text.dim.paint(), p))
    .child(
        text("What's new")
            .grow(1.0)
            .min_w(0.0)
            .font(p.ty.label.medium())
            .color(p.roles.text.text.paint())
            .truncate()
            .a11y_hidden(),
    )
    // The unread dot sits where a row's chevron or check would.
    .child(grid::trail_slot(
        unread.then(|| {
            el().size(s.s2, s.s2)
                .radius(p.radius.full)
                .fill(crate::ui::news::unread_tone(p))
        }),
        grid::trail_w(p),
        p,
    ))
}

fn action_row(desk: &Desk, action: Shortcut, icon: &'static rune::Icon, p: &Look) -> El {
    let Some(command) = keys::command(action) else {
        return el();
    };
    let destructive = action == Shortcut::CloseTerminal;
    let needs_pane = matches!(
        action,
        Shortcut::Find
            | Shortcut::SplitRight
            | Shortcut::SplitDown
            | Shortcut::RenameTab
            | Shortcut::CloseTerminal
    );
    let disabled = needs_pane && desk.connection.shown.is_none();
    let ink = match (disabled, destructive) {
        (true, _) => p.roles.text.faint.paint(),
        (false, true) => p.status.state(State::Failed).tone.ink,
        (false, false) => p.roles.text.text.paint(),
    };
    let glyph_ink = if disabled || destructive {
        ink
    } else {
        p.roles.text.dim.paint()
    };
    let row = control(
        &format!("phone-action-{action:?}"),
        command.title,
        p,
        move |d, tx| d.phone_action(action, tx),
    )
    .w_full()
    .gap(p.space.s2)
    .px(grid::inset(p))
    .child(lead_icon(icon, glyph_ink, p))
    .child(
        text(command.title.to_owned())
            .font(p.ty.label.medium())
            .color(ink)
            .truncate()
            .a11y_hidden(),
    );
    if disabled { row.inert() } else { row }
}
