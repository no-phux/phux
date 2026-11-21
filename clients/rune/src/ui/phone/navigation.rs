//! Touch-first session browsing and contextual actions, separate from search
//! commands. Ordinary chrome stays fixed; large text scrolls the whole body.
//!
//! One inset and rhythm for the page, cards and their touch rows, all from
//! the phone's tokens: the page margin, `xl` cards with rows nested inside
//! them, touch-height rows and the session dock, the phone's one pane of
//! glass.

use crate::ui::tokens::Voice;
use rune::{El, Key, Tx, col, el, row, text, text_input};

use super::{Desk, Look};
use crate::addressable::Addressable;
use crate::desk::phone::navigation::Page;
use crate::ui::tokens::Paint;
use crate::ui::{icons, pressable};

mod actions;
mod card;
mod connections;
mod dock;
mod resource;
mod states;
use crate::ui::Cover;
use actions::actions;
use card::session_card;
use connections::view as connections;
use resource::resource_row;
use states::empty_state;

pub(in crate::ui) fn view(desk: &Desk, top: f32) -> Option<El> {
    let page = desk.phone_page()?;
    let p = &super::page_look(desk);
    let s = &p.space;
    let title = match page {
        Page::Sessions => "Sessions",
        Page::Actions => "Terminal actions",
    };
    let heading = text(title)
        .font(
            (if page == Page::Sessions {
                p.ty.title
            } else {
                p.ty.lead
            })
            .semibold(),
        )
        .color(p.roles.text.text.paint())
        .role("heading")
        .wrap()
        .min_w(0.0)
        .shrink();
    let done = control("phone-navigation-close", "Done", p, |d, _| {
        d.close_phone_navigation()
    })
    .radius(p.radius.full)
    .child(
        text("Done")
            .font(p.ty.label.medium())
            .color(p.roles.accent.accent_text.paint())
            .a11y_hidden(),
    );
    let header = row()
        .w_full()
        .max_w(column_max())
        .min_h(s.s16)
        .pl(crate::ui::tokens::phone::page_margin())
        .pr(s.s1)
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
    };
    Some(
        col()
            .key("phone-navigation-page")
            .cover()
            .items_center()
            .pt(top)
            .fill(p.roles.layer.ground.paint())
            .modal(true, Desk::close_phone_navigation)
            .on_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
            .on_paste(|_: &mut Desk, _, _| {})
            .label(title)
            .child(header)
            .child(body.max_w(column_max())),
    )
}

/// The widest a phone column grows (an iPad's is centred).
fn column_max() -> f32 {
    phux_design::tokens::LAYOUT.content_max.phone_column
}

/// A row nested in a card: the card's radius less its inset.
fn nested(p: &Look) -> f32 {
    (p.radius.xl - p.space.s1).max(p.radius.xs)
}

/// Two lines of text in a touch row.
fn row_h(p: &Look) -> f32 {
    p.space.row_h_touch + p.space.s4
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
    let search = text_input(&phone.query, p.ty.lead)
        .key("phone-session-query")
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .min_h(s.touch_min)
        .py(s.s2)
        .label("Search sessions, panes, agents, and folders")
        .color(ink)
        .placeholder("Search sessions or panes", dim)
        .on_edit(|d: &mut Desk| &mut d.phone.as_mut().expect("phone").query)
        .on_input_change(Desk::phone_search_edited)
        .on_input_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
        .on_submit(|d: &mut Desk, tx| d.blur_phone_search(tx));
    let search = row()
        .w_full()
        .px(s.s3)
        .gap(s.s2)
        .items_center()
        .radius(p.radius.lg)
        .continuous_corners()
        .fill(p.roles.layer.raised.paint())
        // Increased contrast keeps the field's edge; otherwise the fill
        // alone reads as a field, as a system search field does.
        .border(
            s.hairline,
            if p.increased_contrast {
                p.roles.border.control.paint()
            } else {
                p.roles.border.muted.paint()
            },
        )
        // The magnifier grows with the field's text, or the largest size
        // leaves a speck beside a headline-sized placeholder.
        .child(
            rune::icon(&icons::SEARCH, p.icon.grid * p.type_scale)
                .color(dim)
                .a11y_hidden(),
        )
        .child(search);
    let list = session_list(desk, p);
    let large_text = p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale;
    let footer = dock::footer(desk, phone.query.text.trim(), ink, large_text, p);
    let connections = (!phone.query.focused).then(|| connections(desk, p));
    // At large text sizes these controls can each be taller than the
    // ordinary list viewport. Let the whole body scroll, including search
    // and New session, rather than pinning oversized chrome over the list.
    if large_text {
        return col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .px(crate::ui::tokens::phone::page_margin())
            .pb(s.s3)
            .child(
                col()
                    .key("phone-session-list")
                    .w_full()
                    .h_full()
                    .min_h(0.0)
                    .scroll(rune::ScrollSpec {
                        stick_bottom: false,
                    })
                    .gap(s.s3)
                    .child(search)
                    .children(connections)
                    .child(list)
                    .child(footer),
            );
    }
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .px(crate::ui::tokens::phone::page_margin())
        .gap(s.s2)
        .pb(s.s3)
        // Search borrows the machine line's room so even a 320×568 phone
        // can show a complete result above the keyboard and floating dock.
        .child(search)
        .children(connections)
        .child(
            el().grow(1.0)
                .min_h(0.0)
                .w_full()
                .child(
                    col()
                        .key("phone-session-list")
                        .w_full()
                        .h_full()
                        .min_h(0.0)
                        .scroll(rune::ScrollSpec { stick_bottom: false })
                        // Cards slide under the search field and the dock
                        // instead of being cut by a hard edge.
                        .scroll_edge_fade(s.s4)
                        .scroll_margin(s.s4)
                        // A real scroll tail: the last row can clear the
                        // floating dock, also when the search keyboard is up
                        // and at the largest text sizes, where the dock's
                        // label line outgrows the touch minimum.
                        .child(
                            list.pt(s.s2)
                                .pb(s.touch_min.max(p.ty.lead.line_px() + s.s4) + s.s3),
                        ),
                )
                .child(footer.pin(Some(0.0), None, Some(0.0), Some(0.0))),
        )
}

/// Every query word may match a different inventory field; never screen text.
fn matches(query: &str, fields: &[&str]) -> bool {
    let haystack = fields.join(" ").to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

fn resource_matches(desk: &Desk, resource: &Addressable, query: &str) -> bool {
    let pane = desk
        .connection
        .topology
        .as_ref()
        .and_then(|t| t.pane(&resource.id));
    let cwd = pane
        .and_then(|pane| pane.cwd.as_deref())
        .unwrap_or_default();
    let agent = desk.activities.agent_name(&resource.id).unwrap_or_default();
    matches(
        query,
        &[
            &resource.session,
            &resource.label,
            cwd,
            agent,
            desk.machines.label(),
        ],
    )
}

fn session_list(desk: &Desk, p: &Look) -> El {
    let query = &desk.phone.as_ref().expect("phone").query.text;
    let all = desk.addressable();
    let mut list = col().w_full().gap(p.space.s3);
    let mut count = 0;
    for session in desk.ordered_sessions() {
        let members: Vec<_> = all.iter().filter(|r| r.session_id == session.id).collect();
        let filtered: Vec<_> = members
            .iter()
            .copied()
            .filter(|r| resource_matches(desk, r, query))
            .collect();
        if !matches(query, &[&session.name, desk.machines.label()]) && filtered.is_empty() {
            continue;
        }
        count += 1;
        list = list.child(session_card(desk, session, &members, &filtered, p));
    }
    // Hub satellite resources aren't members of a local session, but still
    // belong in the navigator with their host-qualified resource addresses.
    for resource in all.iter().filter(|r| r.id.host().is_some()) {
        if resource_matches(desk, resource, query) {
            count += 1;
            list = list.child(resource_row(desk, resource, p));
        }
    }
    if count == 0 {
        list = list.child(empty_state(desk, query, p));
    }
    other_hosts(list, desk, query, p)
}

/// An inset hairline between touch rows, starting at the text column.
fn separator(p: &Look) -> El {
    let s = &p.space;
    el().w_full()
        .pl(s.s3 + p.icon.grid + s.s2)
        .pr(s.s3)
        .child(
            el().w_full()
                .h(s.hairline)
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
