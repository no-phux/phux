//! Terminal actions: grouped touch lists with an icon per row.
use crate::ui::tokens::Voice;
use phux_design::State;
use rune::{El, col, el, text};

use super::{Desk, Look, card, control, nested, separator};
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
        .px(crate::ui::tokens::phone::page_margin())
        .pt(s.s2)
        .pb(s.s6)
        .gap(s.s4);
    for (index, group) in GROUPS.iter().enumerate() {
        let mut surface = card(p);
        for (i, &(action, icon)) in group.iter().enumerate() {
            if i > 0 {
                surface = surface.child(separator(p));
            }
            surface = surface.child(action_row(desk, action, icon, p));
        }
        // What's new joins the places the app keeps: the top bar has no
        // bell of its own, and ⋯ carries its unread dot.
        if index == NEWS_GROUP {
            surface = surface.child(separator(p)).child(news_row(desk, p));
        }
        list = list.child(surface);
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
    .radius(nested(p))
    .child(
        el().size(p.icon.grid, p.icon.grid)
            .center()
            .child(rune::icon(&icons::BELL, p.icon.size.md).color(p.roles.text.dim.paint()))
            .a11y_hidden(),
    )
    .child(
        text("What's new")
            .grow(1.0)
            .min_w(0.0)
            .font(p.ty.label.medium())
            .color(p.roles.text.text.paint())
            .truncate()
            .a11y_hidden(),
    )
    .children(unread.then(|| {
        el().size(s.s2, s.s2)
            .radius(p.radius.full)
            .fill(crate::ui::news::unread_tone(p))
            .a11y_hidden()
    }))
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
    .radius(nested(p))
    .child(
        el().size(p.icon.grid, p.icon.grid)
            .center()
            .child(rune::icon(icon, p.icon.size.md).color(glyph_ink))
            .a11y_hidden(),
    )
    .child(
        text(command.title.to_owned())
            .font(p.ty.label.medium())
            .color(ink)
            .truncate()
            .a11y_hidden(),
    );
    if disabled { row.inert() } else { row }
}
