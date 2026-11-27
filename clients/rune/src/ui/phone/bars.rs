//! The phone's top bar (back, the place capsule, news, more or the
//! minimap) and its tab pills: 44-point touch targets on opaque surfaces,
//! or the native host's Liquid Glass for the floating round buttons.

use crate::ui::tokens::Voice;
use crate::ui::tokens::layout::PHONE;
use phux_design::State;
use rune::{Color, El, col, el, row, text};

use super::{
    Look, badge, floating_round, glyph, hosted, icons, mark_ink, native_glass, news, spatial,
    surface, touchable,
};
use crate::desk::Desk;
use crate::ui::tokens::Paint;

pub(super) fn top_bar(desk: &Desk, p: &Look) -> El {
    let s = &p.space;
    let session = desk
        .selected_session_name()
        .unwrap_or_else(|| "phux".to_owned());
    // With no terminal on screen the window title is only the app's name:
    // the capsule names the computer instead.
    let title = if desk.connection.shown.is_some() {
        pane_title(desk)
    } else {
        desk.machines.label().to_owned()
    };
    // Spend the capsule's width on where the person is. A decorative terminal
    // tile left both names as ellipses beside the small phone's split map.
    let capsule = row()
        .key("phone-place")
        .grow(1.0)
        .min_w(0.0)
        .min_h(s.s12)
        .pl(s.s3)
        .pr(s.s4)
        .gap(s.s3)
        .radius(p.radius.full)
        .items_center();
    let name = if desk.connection.shown.is_some() {
        "Switch session"
    } else {
        "Connections"
    };
    let detail = place_detail(desk, &session);
    let detail = match native_machine(desk) {
        Some(machine) => format!("{detail} · {machine}"),
        None => detail,
    };
    let tone = focused_tone(desk, p);
    let pressed = |d: &mut Desk| {
        if d.connection.shown.is_some() {
            d.open_phone_switcher();
        } else {
            d.toggle_machines();
        }
    };
    // The capsule is glass with the round buttons beside it, so the bar
    // reads as one material on the native host.
    let place = if native_glass(desk, p) {
        let control = crate::mobile::Control {
            key: "phone-place".to_owned(),
            label: name.to_owned(),
            title: Some(title),
            subtitle: Some((detail, tone)),
            leading: true,
            symbol: "chevron.down",
            enabled: true,
            dark: p.dark(),
            ink: p.roles.text.text.paint(),
            badge: None,
            painted: false,
        };
        hosted(desk, capsule.label(name), control).on_press(pressed)
    } else {
        touchable(surface(capsule, p), p)
            .label(name)
            .on_press(pressed)
            .child(
                col()
                    .min_w(0.0)
                    .grow(1.0)
                    .child(
                        text(title)
                            .w_full()
                            .font(p.ty.lead.semibold())
                            .color(p.roles.text.text.paint())
                            .truncate(),
                    )
                    .child(
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
                            .child(glyph(
                                &icons::CHEVRON,
                                p.icon.size.sm,
                                p.roles.text.dim.paint(),
                            )),
                    ),
            )
    };
    let trailing = spatial::minimap(desk, p).unwrap_or_else(|| {
        floating_round(
            desk,
            "phone-more",
            "Terminal actions",
            &icons::MORE,
            "ellipsis",
            None,
            p,
        )
        .on_press(Desk::open_phone_actions)
    });
    let unread = desk.news.unread.then(|| news::unread_tone(p));
    let bell = floating_round(
        desk,
        "phone-news",
        news::bell_name(desk),
        &icons::BELL,
        "bell",
        unread,
        p,
    )
    .expanded(desk.news.open)
    .on_press(Desk::toggle_news);
    row()
        .key("phone-top")
        .w_full()
        .px(s.s3)
        .pb(s.s2)
        .gap(s.s2)
        .items_center()
        .window_drag()
        .child(
            floating_round(
                desk,
                "phone-back",
                "Sessions",
                &icons::CHEVRON_LEFT,
                "chevron.left",
                None,
                p,
            )
            .on_press(Desk::open_phone_sessions),
        )
        .child(place)
        .child(bell)
        .child(trailing)
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
        .on_press(move |d: &mut Desk| {
            d.close_phone_navigation();
            d.toggle_machines();
        })
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
