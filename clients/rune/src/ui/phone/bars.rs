//! The phone's top bar (back, the place capsule, news, more or the
//! minimap) and its tab pills: 44-point touch targets on opaque surfaces.

use phux_design::State;
use rune::{Color, El, col, el, row, text};

use super::{
    Look, badge, glyph, icons, mark_ink, medium, news, round, spatial, strong, surface, touchable,
    word,
};
use crate::desk::Desk;
use crate::ui::tokens::Paint;

pub(super) fn top_bar(desk: &Desk, p: &Look) -> El {
    let s = &p.space;
    let session = desk
        .selected_session_name()
        .unwrap_or_else(|| "phux".to_owned());
    let full = desk.window_title();
    // With no terminal on screen the window title is only the app's name:
    // the capsule names the computer instead.
    let title = if desk.connection.shown.is_some() {
        full.strip_suffix(&format!(" — {session}"))
            .unwrap_or(full)
            .to_owned()
    } else {
        desk.machines.label().to_owned()
    };
    let tile = el()
        .size(s.s8, s.s8)
        .radius(p.radius.md)
        .continuous_corners()
        .center()
        .fill(p.roles.wash.current.paint())
        .child(glyph(&icons::PROMPT, p.icon.size.md, focused_tone(desk, p)));
    let capsule = row()
        .key("phone-place")
        .grow(1.0)
        .min_w(0.0)
        .min_h(s.s12)
        .pl(s.s2)
        .pr(s.s4)
        .gap(s.s3)
        .radius(p.radius.full)
        .items_center();
    let place = touchable(surface(capsule, p), p)
        .label(if desk.connection.shown.is_some() {
            "Choose pane"
        } else {
            "Connections"
        })
        .on_press(|d: &mut Desk| {
            if d.connection.shown.is_some() {
                d.phone_picker();
            } else {
                d.toggle_machines();
            }
        })
        .child(tile)
        .child(
            col()
                .min_w(0.0)
                .shrink()
                .child(
                    text(title)
                        .font(strong(p.ty.lead))
                        .color(p.roles.text.text.paint())
                        .truncate(),
                )
                .child(
                    row()
                        .gap(s.s1)
                        .items_center()
                        .min_w(0.0)
                        .child(
                            text(place_detail(desk, &session))
                                .font(p.ty.caption)
                                .color(p.roles.text.dim.paint())
                                .truncate(),
                        )
                        .child(glyph(
                            &icons::CHEVRON,
                            p.icon.size.sm,
                            p.roles.text.dim.paint(),
                        )),
                ),
        );
    let ink = p.roles.text.text.paint();
    let trailing = spatial::minimap(desk, p).unwrap_or_else(|| {
        round(
            "phone-more",
            "Terminal actions",
            glyph(&icons::MORE, p.icon.size.lg, ink),
            p,
        )
        .on_press(Desk::open_phone_actions)
    });
    // The unread dot sits on the bell's upper trailing shoulder.
    let dot = (p.round() - s.s4, s.s2);
    let bell = round(
        "phone-news",
        news::bell_name(desk),
        glyph(&icons::BELL, p.icon.size.lg, ink),
        p,
    )
    .expanded(desk.news.open)
    .on_press(Desk::toggle_news)
    .children(news::unread_dot(desk, p, dot, s.s2));
    row()
        .key("phone-top")
        .w_full()
        .px(s.s3)
        .pb(s.s2)
        .gap(s.s2)
        .items_center()
        .window_drag()
        .child(
            round(
                "phone-back",
                "Sessions",
                glyph(&icons::CHEVRON_LEFT, p.icon.size.lg, ink),
                p,
            )
            .on_press(Desk::open_phone_sessions),
        )
        .child(place)
        .child(bell)
        .child(trailing)
}

fn focused_tone(desk: &Desk, p: &Look) -> Color {
    desk.connection.shown.as_ref().map_or_else(
        || p.roles.text.dim.paint(),
        |shown| mark_ink(desk.activities.mark(&shown.resource), p),
    )
}

fn place_detail(desk: &Desk, session: &str) -> String {
    let state = desk
        .connection
        .shown
        .as_ref()
        .map(|shown| word(desk.activities.mark(&shown.resource)));
    match state.filter(|word| !word.is_empty()) {
        Some(word) => format!("{session} · {word}"),
        None => session.to_owned(),
    }
}

/// The session's tabs by name, and a new one. A lone tab needs no strip:
/// its rows go to the terminal, and New Tab is in the commands.
pub(super) fn tab_pills(desk: &Desk, p: &Look) -> Option<El> {
    if desk.connection.tabs.list.len() < 2 {
        return None;
    }
    let s = &p.space;
    let mut pills = surface(
        row()
            .key("phone-tabs")
            .shrink()
            .min_w(0.0)
            .padding(s.s0_5)
            .gap(s.s0_5)
            .radius(p.radius.full)
            .items_center()
            .scroll_x()
            // A tab past the edge fades out instead of being cut square,
            // and one scrolled to stops clear of the fade.
            .scroll_edge_fade(s.s5)
            .scroll_margin(s.s5),
        p,
    );
    for (index, tab) in desk.connection.tabs.list.iter().enumerate() {
        pills = pills.child(tab_pill(desk, index, tab, p));
    }
    let new = round(
        "phone-tab-new",
        "New tab",
        glyph(&icons::PLUS, p.icon.grid, p.roles.text.text.paint()),
        p,
    )
    .on_press(Desk::new_tab);
    let strip = row()
        .w_full()
        .px(s.s3)
        .py(s.s1)
        .gap(s.s2)
        .center()
        .child(pills)
        .child(new);
    // A modal owns the current group until it closes.
    Some(if desk.modal().is_some() {
        strip.inert()
    } else {
        strip
    })
}

/// One tab: a 44-point pill, selected the one way (§1.10), with the
/// needs-you mark and word when one of its terminals asks.
fn tab_pill(desk: &Desk, index: usize, tab: &crate::desk::Tab, p: &Look) -> El {
    let s = &p.space;
    let waiting = tab
        .resources
        .iter()
        .any(|id| desk.activities.mark(id).needs_you());
    let ink = if tab.active {
        p.roles.text.text
    } else {
        p.roles.text.dim
    };
    let name = if waiting {
        format!(
            "Tab {}, {}",
            index + 1,
            State::NeedsYou.word().unwrap_or_default()
        )
    } else {
        format!("Tab {}", index + 1)
    };
    let pill = row()
        .key(format!("phone-tab-{index}"))
        .h(s.touch_min)
        .min_w(s.touch_min)
        // Two of the grid's largest steps: a long name truncates.
        .max_w(s.s16 * 2.0)
        .px(s.s3)
        .gap(s.icon_gap)
        .radius(p.radius.full)
        .items_center()
        .motion(p.motion.select.enter)
        .label(name)
        .on_press(move |d: &mut Desk| d.select_tab(index))
        .children(waiting.then(|| {
            badge::mark(p, State::NeedsYou, p.icon.size.sm).a11y_hidden()
        }))
        .child(
            text(desk.tab_label(tab))
                .font(medium(p.ty.label))
                .color(ink.paint())
                .truncate(),
        );
    let pill = touchable(pill, p);
    if tab.active {
        crate::ui::lifted(pill, p)
    } else {
        pill
    }
}
