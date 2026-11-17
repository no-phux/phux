//! Touch-first session browsing and contextual actions, separate from search
//! commands. Keep chrome fixed while only the inventory scrolls.
//!
//! One inset and rhythm for the page, cards and their touch rows, all from
//! the phone's tokens: the page margin, `xl` cards with rows nested inside
//! them, touch-height rows and the session dock, the phone's one pane of
//! glass (§1.9).

use phux_design::State;
use rune::{Color, El, Key, Tx, col, el, row, text, text_input};

use super::{Desk, Look, medium, strong};
use crate::addressable::Addressable;
use crate::desk::phone::navigation::Page;
use crate::keys::{self, Shortcut};
use crate::ui::tokens::{Paint, Tier};
use crate::ui::{badge, icons, pressable};

mod resource;
use crate::ui::Cover;
use resource::resource_row;

pub(in crate::ui) fn view(desk: &Desk, top: f32) -> Option<El> {
    let page = desk.phone_page()?;
    let p = &super::page_look(desk);
    let s = &p.space;
    let title = match page {
        Page::Sessions => "Sessions",
        Page::Actions => "Terminal actions",
    };
    let header = row()
        .w_full()
        .max_w(column_max())
        .min_h(s.s16)
        .pl(crate::ui::tokens::phone::page_margin())
        .pr(s.s1)
        .gap(s.s2)
        .items_center()
        .child(
            text(title)
                .font(strong(if page == Page::Sessions {
                    p.ty.title
                } else {
                    p.ty.lead
                }))
                .color(p.roles.text.text.paint())
                // At the largest text sizes the title wraps; Done stays.
                .wrap()
                .min_w(0.0)
                .shrink(),
        )
        .child(el().grow(1.0))
        .child(
            control("phone-navigation-close", "Done", p, |d, _| {
                d.close_phone_navigation()
            })
            .radius(p.radius.full)
            .child(
                text("Done")
                    .font(medium(p.ty.label))
                    .color(p.roles.accent.accent_text.paint())
                    .a11y_hidden(),
            ),
        );
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

/// A row nested in a card: the card's radius less its inset (§1.5).
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
            .font(medium(p.ty.label))
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

/// The session dock: glass over the scrolling list, the phone's one
/// backdrop blur (§1.9). Opaque under increased contrast, Reduce
/// Transparency and forced colours.
fn dock(e: El, p: &Look) -> El {
    let glass = phux_design::tokens::MATERIAL.glass;
    let e = if p.glass_allowed() {
        let tint = if p.dark() {
            glass.tint.dark
        } else {
            glass.tint.light
        };
        let layer = match tint.layer {
            "ground" => p.roles.layer.ground,
            "raised" => p.roles.layer.raised,
            "overlay" => p.roles.layer.overlay,
            _ => p.roles.layer.sidebar,
        };
        e.backdrop_blur(glass.blur.phone_bar).fill(Color {
            a: tint.alpha,
            ..layer.paint()
        })
    } else {
        e.fill(p.roles.layer.raised.paint())
    };
    p.elevate(e.continuous_corners(), Tier::Floating)
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
        .radius(p.radius.md)
        .fill(p.roles.layer.raised.paint())
        .border(s.hairline, p.roles.border.control.paint())
        .child(
            rune::icon(&icons::SEARCH, p.icon.grid)
                .color(dim)
                .a11y_hidden(),
        )
        .child(search);
    let list = session_list(desk, p);
    let name = phone.query.text.trim();
    let create = if name.is_empty() {
        "New session".to_owned()
    } else {
        format!("Open or create “{name}”")
    };
    let footer = control("phone-session-new", &create, p, |d, _| {
        d.phone_create_session()
    })
    .w_full()
    .gap(s.s2)
    .center()
    .radius(p.radius.full)
    .child(
        rune::icon(&icons::PLUS, p.icon.size.lg)
            .color(ink)
            .a11y_hidden(),
    )
    .child(
        text(if name.is_empty() {
            "New session"
        } else {
            "Open or create session"
        })
        .key("phone-session-new-label")
        .font(medium(p.ty.label))
        .color(ink)
        .truncate()
        .a11y_hidden(),
    );
    let footer = dock(footer, p);
    let footer = if desk.connection.client.is_some() {
        footer
    } else {
        footer.inert()
    };
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .px(crate::ui::tokens::phone::page_margin())
        .gap(s.s3)
        .pb(s.s3)
        .child(search)
        // Search borrows the machine row's room so even a 320×568 phone
        // can show a complete result above the keyboard and floating dock.
        .children((!phone.query.focused).then(|| connections(desk, p)))
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
                        // A real scroll tail: the last row can clear the
                        // floating dock, also when the search keyboard is up.
                        .child(list.pb(s.touch_min + s.s3)),
                )
                .child(footer.pin(Some(0.0), None, Some(0.0), Some(0.0))),
        )
}

fn connections(desk: &Desk, p: &Look) -> El {
    let dim = p.roles.text.dim.paint();
    let label = format!(
        "Connections · {} · {}",
        desk.machines.label(),
        desk.link_word()
    );
    control("phone-session-connections", &label, p, |d, tx| {
        d.phone_action(Shortcut::Machines, tx)
    })
    .w_full()
    .gap(p.space.s2)
    .fill(p.roles.layer.raised.paint())
    .child(
        text(desk.machines.label().to_owned())
            .font(p.ty.mono)
            .color(dim)
            .truncate()
            .a11y_hidden(),
    )
    .child(el().grow(1.0))
    .child(
        text(desk.link_word())
            .font(p.ty.caption)
            .color(dim)
            .a11y_hidden(),
    )
    .child(
        rune::icon(&icons::CHEVRON_RIGHT, p.icon.size.md)
            .color(dim)
            .a11y_hidden(),
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

fn empty_state(desk: &Desk, query: &str, p: &Look) -> El {
    let (title, detail) = if desk.connection.topology.is_none() {
        (
            "Loading sessions",
            "Your sessions will appear when connected.",
        )
    } else if query.trim().is_empty() {
        ("No sessions yet", "Create a session to start something.")
    } else {
        ("No matches", "Try a session, pane, agent, or folder.")
    };
    col()
        .w_full()
        .padding(p.space.s3)
        .gap(p.space.s2)
        .child(
            text(title)
                .font(strong(p.ty.lead))
                .color(p.roles.text.text.paint()),
        )
        .child(
            text(detail)
                .font(p.ty.body)
                .color(p.roles.text.dim.paint())
                .wrap(),
        )
}

fn session_card(
    desk: &Desk,
    session: &phux_client_runtime::control::SessionDescriptor,
    members: &[&Addressable],
    filtered: &[&Addressable],
    p: &Look,
) -> El {
    let s = &p.space;
    let dim = p.roles.text.dim.paint();
    let waiting = members
        .iter()
        .filter(|r| desk.activities.mark(&r.id).needs_you())
        .count();
    let panes = match members.len() {
        1 => "1 pane".to_owned(),
        n => format!("{n} panes"),
    };
    let name = session.name.clone();
    let current = desk.selected_session_name().as_deref() == Some(&session.name);
    let attention = phux_design::plural("count.needs_you", waiting as u64).unwrap_or_default();
    let label = format!(
        "{} · {panes}{} · {attention}",
        session.name,
        if current { " · current session" } else { "" },
    );
    let header = control(
        &format!("phone-session-{}", session.id),
        &label,
        p,
        move |d, _| d.phone_session(&name),
    )
    .w_full()
    .min_h(row_h(p))
    .gap(s.s2)
    .radius(nested(p))
    .child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(s.s1)
            .child(
                row()
                    .min_w(0.0)
                    .gap(s.icon_gap)
                    .items_center()
                    .child(
                        text(session.name.clone())
                            .font(strong(p.ty.lead))
                            .color(p.roles.text.text.paint())
                            .truncate()
                            .a11y_hidden(),
                    )
                    .children(current.then(|| {
                        rune::icon(&icons::CHECK, p.icon.size.md)
                            .color(dim)
                            .a11y_hidden()
                    })),
            )
            .child(text(panes).font(p.ty.caption).color(dim).a11y_hidden()),
    )
    .children(
        (waiting > 0).then(|| badge::tone_badge(p, State::NeedsYou, Some(attention)).a11y_hidden()),
    );
    let mut card = col()
        .w_full()
        .padding(s.s1)
        .radius(p.radius.xl)
        .continuous_corners()
        .fill(p.roles.layer.raised.paint())
        .child(header);
    for resource in filtered {
        card = card.child(resource_row(desk, resource, p));
    }
    card
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

fn actions(desk: &Desk, p: &Look) -> El {
    let mut list = col()
        .w_full()
        .px(crate::ui::tokens::phone::page_margin())
        .py(p.space.s3)
        .gap(p.space.s1);
    for action in [
        Shortcut::NewTerminal,
        Shortcut::NewSession,
        Shortcut::Find,
        Shortcut::SplitRight,
        Shortcut::SplitDown,
        Shortcut::RenameTab,
        Shortcut::RenameSession,
        Shortcut::Inbox,
        Shortcut::Machines,
        Shortcut::Settings,
        Shortcut::CloseTerminal,
    ] {
        let Some(command) = keys::command(action) else {
            continue;
        };
        let control = button(
            &format!("phone-action-{action:?}"),
            command.title,
            p,
            move |d, tx| d.phone_action(action, tx),
        )
        .w_full();
        let needs_pane = matches!(
            action,
            Shortcut::Find
                | Shortcut::SplitRight
                | Shortcut::SplitDown
                | Shortcut::RenameTab
                | Shortcut::CloseTerminal
        );
        list = list.child(if needs_pane && desk.connection.shown.is_none() {
            control.inert()
        } else {
            control
        });
    }
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(list)
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
