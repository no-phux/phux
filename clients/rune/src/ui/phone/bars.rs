//! The phone's top bar (back, where you are, more or the minimap) and its
//! tab pills: 44-point touch targets, on opaque surfaces or the native
//! host's Liquid Glass for the floating round buttons.

use crate::ui::tokens::Voice;
use crate::ui::tokens::layout::PHONE;
use phux_design::State;
use rune::{Color, El, col, el, row, text};

use super::{
    Look, badge, floating_round, glyph, hosted, icons, mark_ink, news, spatial, surface, touchable,
};
use crate::desk::Desk;
use crate::ui::tokens::Paint;

pub(super) fn top_bar(desk: &Desk, p: &Look) -> El {
    let s = &p.space;
    let session = desk
        .selected_session_name()
        .unwrap_or_else(|| "phux".to_owned());
    // With no terminal on screen the window title is only the app's name:
    // the title names the computer instead.
    let title = if desk.connection.shown.is_some() {
        pane_title(desk)
    } else {
        desk.machines.label().to_owned()
    };
    let detail = place_detail(desk, &session);
    let detail = match native_machine(desk) {
        Some(machine) => format!("{detail} · {machine}"),
        None => detail,
    };
    let place = place(desk, title, detail, p);
    // What's new lives behind ⋯ (Terminal actions); its unread dot rides
    // on ⋯, so the bar is back, where you are, and more: nothing else.
    let unread = desk.news.unread.then(|| news::unread_tone(p));
    let trailing = spatial::minimap(desk, p).unwrap_or_else(|| {
        floating_round(
            desk,
            "phone-more",
            if unread.is_some() {
                "Terminal actions, What's new has updates"
            } else {
                "Terminal actions"
            },
            &icons::MORE,
            "ellipsis",
            unread,
            p,
        )
        .on_press(Desk::open_phone_actions)
    });
    row()
        .key("phone-top")
        .w_full()
        .px(s.s3)
        .pb(s.s1)
        .gap(s.s1)
        .items_center()
        .window_drag()
        .child(back(desk, p))
        .child(place)
        .child(trailing)
}

/// Back to Sessions, carrying the needs-you dot while a pane off screen
/// asks: the way to it is one tap from the terminal, whatever it shows.
fn back(desk: &Desk, p: &Look) -> El {
    let waiting = waiting_elsewhere(desk);
    let label = match waiting {
        0 => "Sessions".to_owned(),
        n => format!(
            "Sessions, {}",
            phux_design::plural("count.needs_you", n as u64).unwrap_or_default()
        ),
    };
    let dot = (waiting > 0).then(|| p.status.state(State::NeedsYou).tone.ink);
    floating_round(
        desk,
        "phone-back",
        &label,
        &icons::CHEVRON_LEFT,
        "chevron.left",
        dot,
        p,
    )
    .on_press(Desk::open_phone_sessions)
}

/// Panes that need you other than the one on screen, whose own state the
/// title's subtitle already says.
fn waiting_elsewhere(desk: &Desk) -> usize {
    let shown = desk.connection.shown.as_ref().map(|s| &s.resource);
    desk.addressable()
        .iter()
        .filter(|r| Some(&r.id) != shown && desk.activities.mark(&r.id).needs_you())
        .count()
}

/// Where you are, as an iOS navigation title: the pane over its session,
/// state and computer, plain on the terminal's ground rather than boxed.
/// One touch target tall; a tap opens the session switcher (Connections
/// with no pane on screen).
fn place(desk: &Desk, title: String, detail: String, p: &Look) -> El {
    let s = &p.space;
    let name = if desk.connection.shown.is_some() {
        "Switch session"
    } else {
        "Connections"
    };
    let tone = focused_tone(desk, p);
    // A pane with no title of its own leads with the line below it.
    let (title, detail) = if title.trim().is_empty() {
        (detail, None)
    } else {
        (title, Some(detail))
    };
    let lines = col()
        .min_w(0.0)
        .shrink()
        .child(
            row()
                .w_full()
                .gap(s.s1)
                .items_center()
                .min_w(0.0)
                .child(
                    text(title)
                        .min_w(0.0)
                        .shrink()
                        .font(p.ty.lead.semibold())
                        .color(p.roles.text.text.paint())
                        .truncate(),
                )
                .children(detail.is_none().then(|| chevron(p))),
        )
        .children(detail.map(|detail| {
            row()
                .w_full()
                .gap(s.s1)
                .items_center()
                .min_w(0.0)
                .child(
                    text(detail)
                        .min_w(0.0)
                        .shrink()
                        .font(p.ty.caption)
                        .color(tone)
                        .truncate(),
                )
                .child(chevron(p))
        }));
    let place = row()
        .key("phone-place")
        .grow(1.0)
        .min_w(0.0)
        .min_h(p.round())
        .px(s.s2)
        .radius(p.radius.md)
        .continuous_corners()
        .items_center()
        .label(name)
        .on_press(|d: &mut Desk| {
            if d.connection.shown.is_some() {
                d.open_phone_switcher();
            } else {
                d.toggle_machines();
            }
        })
        .child(lines);
    touchable(place, p)
}

fn chevron(p: &Look) -> El {
    glyph(&icons::CHEVRON, p.icon.size.sm, p.roles.text.dim.paint())
}

pub(super) fn pane_title(desk: &Desk) -> String {
    let full = desk.window_title();
    desk.selected_session_name()
        .and_then(|session| full.strip_suffix(&format!(" — {session}")))
        .unwrap_or(full)
        .to_owned()
}

/// The computer's pill, shown in the session switcher: tapping it opens
/// Connections. The terminal screen spends no row on it; the top bar's
/// subtitle names the computer instead.
pub(super) fn computer(desk: &Desk, p: &Look) -> El {
    let machine = desk.machines.label();
    let s = &p.space;
    let dot = s.s2;
    let pill = row()
        .max_w(PHONE.tab_max_w)
        .min_h(s.s8)
        .px(s.s3)
        .gap(s.s2)
        .items_center()
        .radius(p.radius.full)
        .child(
            el().size(dot, dot).radius(p.radius.full).fill(
                p.status
                    .state(if desk.link_word() == "connected" {
                        State::Done
                    } else {
                        State::Working
                    })
                    .tone
                    .ink,
            ),
        )
        .child(
            text(machine.to_owned())
                .min_w(0.0)
                .shrink()
                .font(p.ty.label.medium())
                .color(p.roles.text.text.paint())
                .truncate(),
        )
        .child(glyph(
            &icons::CHEVRON,
            p.icon.size.sm,
            p.roles.text.dim.paint(),
        ));
    let label = format!("Connections · {machine} · {}", desk.link_word());
    let key = "phone-switcher-computer";
    let hit = el()
        .key(key)
        .min_h(p.round())
        .min_w(p.round())
        .center()
        .label(&label)
        .on_press_tx(|d: &mut Desk, tx| d.phone_connections(tx))
        .child(surface(pill, p));
    row().w_full().center().child(hosted(
        desk,
        touchable(hit, p),
        crate::mobile::Control::painted(key, &label, true),
    ))
}

/// The computer a native host is attached to, once a terminal is on screen.
/// The desktop-hosted phone is the computer it runs on.
fn native_machine(desk: &Desk) -> Option<&str> {
    (desk.native.is_some() && desk.connection.shown.is_some()).then(|| desk.machines.label())
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
        .map(|shown| desk.activities.mark(&shown.resource).word());
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
    let new = floating_round(
        desk,
        "phone-tab-new",
        "New tab",
        &icons::PLUS,
        "plus",
        None,
        p,
    )
    .on_press(Desk::new_tab);
    // One touch target tall: the strip spends no rows the terminal could.
    let strip = row()
        .key("phone-tab-strip")
        .w_full()
        .px(s.s3)
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

/// One tab: a 44-point pill, selected the one way, with the
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
        .max_w(PHONE.tab_max_w)
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
                .font(p.ty.label.medium())
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
