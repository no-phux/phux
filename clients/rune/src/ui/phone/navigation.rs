//! Touch-first session browsing and contextual actions, separate from search
//! commands. Ordinary chrome stays fixed; large text scrolls the whole body.
//!
//! Sessions share one canvas: session names anchor indented pane lists.
//! Actions retain grouped cards. Both use touch-height rows.

use crate::ui::tokens::Voice;
use rune::{El, Key, Tx, col, el, row, text, text_input};

use super::{Desk, Look};
use crate::desk::phone::navigation::Page;
use crate::ui::tokens::{GlassSurface, Paint};
use crate::ui::{icons, pressable};

mod actions;
mod connections;
mod dock;
mod grid;
mod inventory;
mod menu;
mod resource;
mod section;
mod states;
mod switcher;
use crate::ui::Cover;
use actions::actions;
use connections::view as connections;
use inventory::{matches, session_list};
pub(crate) use menu::native as native_menu;
use resource::resource_row;
use states::empty_state;

pub(in crate::ui) fn view(desk: &Desk, top: f32) -> Option<El> {
    let phone = desk.phone.as_ref()?;
    // A finger pulling the drawer in from the left edge shows Sessions
    // before it is open: inert, following the finger, not yet a modal.
    let pull = phone.swipe.pull.filter(|_| desk.phone_page().is_none());
    let page = desk.phone_page().or(pull.map(|_| Page::Sessions))?;
    let p = &super::page_look(desk);
    let s = &p.space;
    if page == Page::Switcher {
        return Some(switcher::view(desk, top, p));
    }
    let title = match page {
        Page::Sessions => "Sessions",
        Page::Actions => "Terminal actions",
        Page::Switcher => unreachable!(),
    };
    let heading = text(title)
        .key("phone-navigation-heading")
        .font(p.ty.title.semibold())
        .color(p.roles.text.text.paint())
        .role("heading")
        .wrap()
        .min_w(0.0)
        .shrink();
    let glass_done = super::native_glass(desk, p);
    let done = done(desk, p);
    // The title starts on the cards' edge; Done ends on it, as a bar
    // button does: the glass circle itself, or the accent word inside its
    // touch box.
    let margin = grid::margin();
    let header = row()
        .w_full()
        .max_w(column_max())
        .min_h(s.touch_min + s.s2)
        .pl(margin)
        .pr(if glass_done { margin } else { margin - s.s3 })
        .gap(s.s2);
    // At the accessibility text sizes the title takes its own line under
    // Done. Searching borrows that large title's room above the keyboard;
    // the modal keeps its Sessions name and Done stays in reach.
    let searching =
        page == Page::Sessions && desk.phone.as_ref().is_some_and(|phone| phone.query.focused);
    let header = if p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale {
        header
            .flex_wrap()
            .child(el().grow(1.0))
            .child(done)
            .children((!searching).then(|| el().w_full().child(heading)))
    } else {
        header
            .items_center()
            .child(heading)
            .child(el().grow(1.0))
            .child(done)
    };
    let body = match page {
        Page::Sessions => sessions(desk, p),
        Page::Actions => actions(desk, p),
        Page::Switcher => unreachable!(),
    };
    let sheet = col()
        .key("phone-navigation-page")
        .cover()
        .items_center()
        .pt(top)
        .on_layout(|d: &mut Desk, rect| {
            if let Some(phone) = &mut d.phone {
                phone.page_size = [rect[2], rect[3]];
            }
        })
        .label(title)
        .child(header)
        .child(body.max_w(column_max()))
        .children(menu::view(desk, p));
    let sheet = drawer(sheet, page, pull, phone.stage[0], p);
    Some(match pull {
        Some(_) => sheet.inert(),
        None => sheet
            .modal(true, Desk::close_phone_navigation)
            .on_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
            .on_paste(|_: &mut Desk, _, _| {}),
    })
}

/// Sessions comes in from the left edge, where the thumb pulls it from, as
/// frost rather than a sliding panel: the terminal blurs in place under
/// [`frost`] (the phone's one blur) while the list drifts a short way in
/// behind the finger and sharpens. A let-go pull or a close fades it back
/// off. Terminal actions is one glass page. Opaque where glass may not draw.
fn drawer(sheet: El, page: Page, pull: Option<f32>, width: f32, p: &Look) -> El {
    if page != Page::Sessions {
        return glass(sheet, p);
    }
    let shown = shown(pull, width);
    let drift = p.space.s16;
    // The list trails the frost, so it reads once the glass has come up
    // behind it rather than as text floating on the terminal.
    sheet
        .offset(-drift * (1.0 - shown), 0.0)
        .offset_motion(follow(pull, p))
        .opacity(shown * shown)
        .opacity_motion(follow(pull, p))
        .transition(
            rune::Transition::offset(-drift, 0.0)
                .combined(rune::Transition::opacity())
                .springs(p.motion.drawer),
        )
}

/// The frost Sessions comes up on: the whole terminal blurred in place.
/// Under a pulling finger it thickens, its blur and tint growing from
/// clear to whole by the point a lift opens, so the terminal never shows
/// crisp through the list. A sibling under the list, never inside a layer
/// that fades as one, which would flatten what it blurs.
pub(in crate::ui) fn frost(desk: &Desk) -> Option<El> {
    let phone = desk.phone.as_ref()?;
    let pull = phone.swipe.pull.filter(|_| desk.phone_page().is_none());
    let page = desk.phone_page().or(pull.map(|_| Page::Sessions))?;
    if page != Page::Sessions {
        return None;
    }
    let p = &super::page_look(desk);
    let commit = crate::desk::phone::swipe::PULL_COMMIT;
    let thick = (shown(pull, phone.stage[0]) / commit).min(1.0);
    let frost = p.glass_panel_thick(
        el().key("phone-drawer-frost").cover(),
        GlassSurface::PhoneBar,
        p.glass_tint_layer(),
        p.roles.layer.ground,
        thick,
    );
    Some(
        frost
            .transition(rune::Transition::opacity().springs(p.motion.drawer))
            .inert()
            .a11y_hidden(),
    )
}

fn glass(panel: El, p: &Look) -> El {
    p.glass_panel(
        panel,
        GlassSurface::PhoneBar,
        p.glass_tint_layer(),
        p.roles.layer.ground,
    )
}

/// How far in Sessions has come: whole once open.
fn shown(pull: Option<f32>, width: f32) -> f32 {
    pull.map_or(1.0, |pulled| (pulled / width.max(1.0)).clamp(0.0, 1.0))
}

/// A pulling finger is followed exactly; a let-go one settles on the
/// drawer spring.
fn follow(pull: Option<f32>, p: &Look) -> rune::Spring {
    if pull.is_some() {
        rune::Spring::NONE
    } else {
        p.motion.drawer.enter
    }
}

/// Done closes the page: a glass checkmark like the top bar's buttons on
/// the native host, inset as they are; Rune's accent word elsewhere.
fn done(desk: &Desk, p: &Look) -> El {
    let done = control("phone-navigation-close", "Done", p, |d, _| {
        d.close_phone_navigation()
    })
    .radius(p.radius.full);
    if !super::native_glass(desk, p) {
        return done.child(
            text("Done")
                .font(p.ty.label.medium())
                .color(p.roles.accent.accent_text.paint())
                .a11y_hidden(),
        );
    }
    let control = crate::mobile::Control {
        key: "phone-navigation-close".to_owned(),
        label: "Done".to_owned(),
        title: None,
        subtitle: None,
        leading: false,
        symbol: "checkmark",
        enabled: true,
        dark: p.dark(),
        ink: p.roles.accent.accent_text.paint(),
        badge: None,
        painted: false,
    };
    super::hosted(desk, done.size(p.round(), p.round()).px(0.0), control)
}

/// The widest a phone column grows (an iPad's is centred).
fn column_max() -> f32 {
    phux_design::tokens::LAYOUT.content_max.phone_column
}

/// A row nested in a padded panel: the panel's radius less its inset.
fn nested(p: &Look) -> f32 {
    (p.radius.xl - p.space.s1).max(p.radius.xs)
}

/// A floating panel of rows, such as the long-press menu: rows inset by
/// [`nested`] so each press keeps its own rounded shape.
fn panel(p: &Look) -> El {
    col()
        .w_full()
        .padding(p.space.s1)
        .radius(p.radius.xl)
        .continuous_corners()
        .fill(p.roles.layer.raised.paint())
}

/// A grouped surface: `rows` edge to edge on one card, an inset hairline
/// between them from the text column, as an iOS inset grouped list draws
/// them. The first and last rows take the card's corners, so a press
/// fills the row to the card's edge.
fn card(rows: impl IntoIterator<Item = El>, p: &Look) -> El {
    let rows: Vec<El> = rows.into_iter().collect();
    let last = rows.len().saturating_sub(1);
    let r = p.radius.xl;
    let mut surface = col()
        .w_full()
        .radius(r)
        .continuous_corners()
        .fill(grid::card_fill(p));
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            surface = surface.child(separator(p));
        }
        let top = if i == 0 { r } else { 0.0 };
        let bottom = if i == last { r } else { 0.0 };
        surface = surface.child(row.corners([top, top, bottom, bottom]).continuous_corners());
    }
    surface
}

fn control(
    key: &str,
    label: &str,
    p: &Look,
    action: impl Fn(&mut Desk, &mut Tx) + Clone + 'static,
) -> El {
    let pressed = action.clone();
    pressable(key, p)
        .min_h(p.space.touch_min)
        .min_w(p.space.touch_min)
        .px(p.space.s3)
        .radius(p.radius.md)
        .items_center()
        .label(label.to_owned())
        .focusable(true)
        .on_press_tx(move |d: &mut Desk, tx| pressed(d, tx))
        .on_key(move |d: &mut Desk, event, tx| {
            if event.key == Key::Enter || event.key == Key::Text(" ".into()) {
                action(d, tx);
                true
            } else {
                false
            }
        })
}

fn button(
    key: &str,
    label: &str,
    p: &Look,
    action: impl Fn(&mut Desk, &mut Tx) + Clone + 'static,
) -> El {
    control(key, label, p, action).child(
        text(label.to_owned())
            .font(p.ty.label.medium())
            .color(p.roles.text.text.paint())
            .truncate()
            .a11y_hidden(),
    )
}

fn note(words: impl Into<String>, p: &Look) -> El {
    el().w_full().px(p.space.s3).py(p.space.s3).child(
        text(words.into())
            .font(p.ty.body)
            .color(p.roles.text.dim.paint()),
    )
}

fn sessions(desk: &Desk, p: &Look) -> El {
    let s = &p.space;
    let phone = desk.phone.as_ref().expect("phone navigation");
    let dim = p.roles.text.dim.paint();
    let ink = p.roles.text.text.paint();
    // The lead role: an iOS field under 16 points zooms the page.
    // The line sits on the field's centre line, with the magnifier: equal
    // room above and below it at every text size.
    let line = p.ty.lead.line_px();
    let magnifier = p.icon.grid * p.type_scale;
    let search = text_input(&phone.query, p.ty.lead)
        .key("phone-session-query")
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .min_h(s.touch_min)
        .py(((s.touch_min - line) / 2.0).max(s.s2))
        .label("Search sessions, panes, agents, and folders")
        .color(ink)
        // Past the grid's text cap the full hint is cut mid-word on a
        // phone; the page's title already says what is searched.
        .placeholder(
            if p.type_scale > crate::desk::accessibility::GRID_CAP {
                "Search"
            } else {
                "Search sessions or panes"
            },
            dim,
        )
        .on_edit(|d: &mut Desk| &mut d.phone.as_mut().expect("phone").query)
        .on_input_change(Desk::phone_search_edited)
        .on_input_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
        .on_submit(|d: &mut Desk, tx| d.blur_phone_search(tx));
    let search = row()
        .key("phone-session-search")
        .w_full()
        .pl(grid::inset(p))
        .pr(grid::clear_inset(p))
        .gap(s.s2)
        .items_center()
        .radius(p.radius.full)
        .continuous_corners()
        .fill(grid::card_fill(p))
        // The magnifier sits in the leading column the rows' marks use,
        // and the text starts where their titles do. Past the grid's cap it
        // keeps growing with the field's text, which would otherwise leave
        // a speck beside a headline-sized placeholder, and widens its slot.
        .child(
            el().size(grid::lead_w(p).max(magnifier), line)
                .center()
                .key("phone-session-search-mark")
                .child(rune::icon(&icons::SEARCH, magnifier).color(dim))
                .a11y_hidden(),
        )
        .child(search)
        .children((!phone.query.text.is_empty()).then(|| {
            control("phone-session-clear", "Clear search", p, Desk::clear_phone_search)
                .px(0.0)
                .center()
                .preserve_focus_on_press(true)
                .child(
                    rune::icon(&icons::CLOSE, p.icon.grid * p.type_scale)
                        .color(dim)
                        .a11y_hidden(),
                )
        }));
    // Increased contrast keeps the field's edge; otherwise the fill alone
    // reads as a field, as a system search field does.
    let search = if p.increased_contrast {
        search.border(s.hairline, p.roles.border.control.paint())
    } else {
        search
    };
    let connections = (!phone.query.focused).then(|| connections(desk, p));
    // Connections leads the list and scrolls with it; only the title and
    // search stay put, as a system list's do.
    let list = col()
        .w_full()
        .gap(grid::block_gap(p))
        .children(connections)
        .child(session_list(desk, p));
    let large_text = p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale;
    let footer = dock::footer(desk, phone.query.text.trim(), ink, large_text, p);
    let margin = grid::margin();
    // The scroller spans the page and its content keeps the margin, so a
    // keyboard ring around a card-wide row is never cut by the viewport.
    let scroller = col()
        .key("phone-session-list")
        .w_full()
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        });
    // At large text sizes these controls can each be taller than the
    // ordinary list viewport. Let the whole body scroll, including search
    // and New session, rather than pinning oversized chrome over the list.
    if large_text {
        return col().w_full().grow(1.0).min_h(0.0).pb(s.s3).child(
            scroller.h_full().scroll_margin(s.s1).child(
                col()
                    .w_full()
                    .px(margin)
                    .pt(s.s1)
                    .gap(grid::block_gap(p))
                    .child(search)
                    .child(list)
                    .child(footer)
                    // A real tail protects the last outside focus ring
                    // where reveal margins cannot scroll farther.
                    .child(el().w_full().h(s.s1).inert().a11y_hidden()),
            ),
        );
    }
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .pb(s.s3)
        .child(el().w_full().px(margin).child(search))
        .child(
            scroller
                .grow(1.0)
                .scroll_edge_fade(s.s4)
                .scroll_margin(s.s2)
                .child(list.px(margin).py(s.s4)),
        )
        .child(el().w_full().px(margin).child(footer))
}

/// An inset hairline between touch rows, from the text column to the
/// card's trailing edge.
fn separator(p: &Look) -> El {
    separator_at(grid::text_x(p), p)
}

fn separator_at(lead: f32, p: &Look) -> El {
    el().w_full()
        .pl(lead)
        .child(
            el().w_full()
                .h(p.space.hairline)
                .fill(p.roles.border.muted.paint()),
        )
        .a11y_hidden()
}

fn other_hosts(mut list: El, desk: &Desk, query: &str, p: &Look) -> El {
    // Native iOS has no local CLI or local server. Connections delegates to
    // UIKit; never offer a phantom “This Mac” whose listing cannot run.
    if desk.native.is_some() {
        return list;
    }
    let local = desk
        .machines
        .current
        .is_some()
        .then_some((None, crate::machines::LOCAL));
    let hosts = desk
        .machines
        .hosts
        .iter()
        .filter(|h| !h.paused && desk.machines.current.as_ref() != Some(&h.name))
        .map(|h| (Some(h.name.clone()), h.name.as_str()));
    for (host, label) in local.into_iter().chain(hosts) {
        let held = host.clone();
        list = list.child(
            button(
                &format!("phone-sessions-host-{label}"),
                label,
                p,
                move |d, _| d.toggle_host(held.clone()),
            )
            .w_full(),
        );
        if !desk.machines.expanded.contains(&host) {
            continue;
        }
        match desk.machines.glances.get(&host) {
            Some(Ok(sessions)) => {
                for session in sessions
                    .iter()
                    .filter(|s| matches(query, &[label, &s.name]))
                {
                    let (host, name) = (host.clone(), session.name.clone());
                    list = list.child(
                        button(
                            &remote_key(&host, &name),
                            &format!("{name} · {} terminals", session.terminals),
                            p,
                            move |d, _| {
                                d.close_phone_navigation();
                                d.open_on(host.clone(), name.clone());
                            },
                        )
                        .w_full(),
                    );
                }
                if sessions.is_empty() {
                    list = list.child(note("No sessions", p));
                }
            }
            Some(Err(error)) => list = list.child(note(error.clone(), p)),
            None => list = list.child(note("Listing…", p)),
        }
    }
    list
}

fn remote_key(host: &Option<String>, session: &str) -> String {
    format!(
        "phone-remote-{}",
        serde_json::to_string(&(host, session)).expect("string tuple")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_row_identity_keeps_host_and_session_boundaries() {
        assert_ne!(
            remote_key(&Some("a".into()), "b-c"),
            remote_key(&Some("a-b".into()), "c")
        );
        assert_ne!(
            remote_key(&None, "work"),
            remote_key(&Some(crate::machines::LOCAL.into()), "work")
        );
    }

    #[test]
    fn session_search_matches_words_across_inventory_fields() {
        assert!(matches(
            "API CLAUDE",
            &["api", "Claude Code", "/src/server"]
        ));
        assert!(matches("日本", &["日本語"]));
        assert!(!matches("api missing", &["api", "Claude Code"]));
    }
}
