//! Touch-first session browsing and contextual actions, separate from search
//! commands. Keep chrome fixed while only the inventory scrolls.

use rune::{El, Key, TextStyle, Tx, col, el, row, text, text_input};

use super::{CAP_WORD, Desk, Ink, ROUND, SUBTITLE, TITLE};
use crate::addressable::Addressable;
use crate::desk::phone::navigation::Page;
use crate::keys::{self, Shortcut};
use crate::ui::{icons, pressable, sidebar};

pub(super) fn view(desk: &Desk, p: &Ink, top: f32) -> Option<El> {
    let page = desk.phone_page()?;
    let title = match page {
        Page::Sessions => "Sessions",
        Page::Actions => "Terminal actions",
    };
    let header = row()
        .w_full()
        .h(56.0)
        .px(12.0)
        .gap(8.0)
        .items_center()
        .child(text(title).font(TITLE).color(p.text))
        .child(el().grow(1.0))
        .child(button("phone-navigation-close", "Done", p, |d, _| {
            d.close_phone_navigation()
        }));
    let body = match page {
        Page::Sessions => sessions(desk, p),
        Page::Actions => actions(desk, p),
    };
    Some(
        col()
            .key("phone-navigation-page")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .pt(top)
            .fill(p.bg)
            .modal(true, Desk::close_phone_navigation)
            .on_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
            .on_paste(|_: &mut Desk, _, _| {})
            .label(title)
            .child(header)
            .child(body),
    )
}

fn control(
    key: &str,
    label: &str,
    p: &Ink,
    action: impl Fn(&mut Desk, &mut Tx) + Clone + 'static,
) -> El {
    let pressed = action.clone();
    pressable(key, p)
        .min_h(ROUND)
        .min_w(ROUND)
        .px(12.0)
        .radius(10.0)
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
    p: &Ink,
    action: impl Fn(&mut Desk, &mut Tx) + Clone + 'static,
) -> El {
    control(key, label, p, action).child(
        text(label.to_owned())
            .font(CAP_WORD)
            .color(p.text)
            .truncate(),
    )
}

fn note(words: impl Into<String>, p: &Ink) -> El {
    el().w_full()
        .px(12.0)
        .py(12.0)
        .child(text(words.into()).font(SUBTITLE).color(p.dim))
}

fn sessions(desk: &Desk, p: &Ink) -> El {
    let phone = desk.phone.as_ref().expect("phone navigation");
    let search = text_input(&phone.query, TextStyle::new(16.0, 400.0))
        .key("phone-session-query")
        .single_line()
        .w_full()
        .min_h(ROUND)
        .px(12.0)
        .radius(12.0)
        .fill(p.raised)
        .color(p.text)
        .placeholder("Search sessions, panes, agents, folders…", p.dim)
        .on_edit(|d: &mut Desk| &mut d.phone.as_mut().expect("phone").query)
        .on_input_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
        .on_submit(|d: &mut Desk, tx| d.blur_phone_search(tx));
    let list = session_list(desk, p);
    let name = phone.query.text.trim();
    let create = if name.is_empty() {
        "New session".to_owned()
    } else {
        format!("Open or create “{name}”")
    };
    let footer = button("phone-session-new", &create, p, |d, _| {
        d.phone_create_session()
    })
    .w_full()
    .fill(p.current);
    let footer = if desk.client.is_some() {
        footer
    } else {
        footer.inert()
    };
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .px(12.0)
        .gap(8.0)
        .pb(8.0)
        .child(search)
        .child(
            col()
                .key("phone-session-list")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(list),
        )
        .child(footer)
}

/// Every query word may match a different inventory field; never screen text.
fn matches(query: &str, fields: &[&str]) -> bool {
    let haystack = fields.join(" ").to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

fn resource_matches(desk: &Desk, resource: &Addressable, query: &str) -> bool {
    let pane = desk.topology.as_ref().and_then(|t| t.pane(&resource.id));
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

fn session_list(desk: &Desk, p: &Ink) -> El {
    let query = &desk.phone.as_ref().expect("phone").query.text;
    let all = desk.addressable();
    let mut list = col().w_full().gap(8.0).child(
        button(
            "phone-session-connections",
            &format!("{} · {}", desk.machines.label(), desk.link_word()),
            p,
            |d, tx| d.phone_action(Shortcut::Machines, tx),
        )
        .w_full(),
    );
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
        list = list.child(note(
            if desk.topology.is_none() {
                "Waiting for the session inventory…"
            } else if query.trim().is_empty() {
                "No sessions here yet. Create one to get started."
            } else {
                "No matching sessions or panes."
            },
            p,
        ));
    }
    other_hosts(list, desk, query, p)
}

fn session_card(
    desk: &Desk,
    session: &phux_client_runtime::control::SessionDescriptor,
    members: &[&Addressable],
    filtered: &[&Addressable],
    p: &Ink,
) -> El {
    let waiting = members
        .iter()
        .filter(|r| desk.activities.mark(&r.id).needs_you())
        .count();
    let panes = match members.len() {
        1 => "1 pane".to_owned(),
        n => format!("{n} panes"),
    };
    let summary = if waiting > 0 {
        format!("{panes} · {waiting} need you")
    } else {
        panes
    };
    let name = session.name.clone();
    let current = desk.selected_session_name().as_deref() == Some(&session.name);
    let header = button(
        &format!("phone-session-{}", session.id),
        &session.name,
        p,
        move |d, _| d.phone_session(&name),
    )
    .w_full()
    .gap(8.0)
    .children(current.then(|| rune::icon(&icons::CHECK, 16.0).color(p.accent)))
    .child(el().grow(1.0))
    .child(
        text(summary)
            .font(SUBTITLE)
            .color(if waiting > 0 { p.marks.alert } else { p.dim }),
    );
    let mut card = col().w_full().radius(12.0).fill(p.raised).child(header);
    for resource in filtered {
        card = card.child(resource_row(desk, resource, p));
    }
    if members.is_empty() {
        card = card.child(note("Empty session · tap to open", p));
    }
    card
}

fn resource_row(desk: &Desk, resource: &Addressable, p: &Ink) -> El {
    let mark = desk.activities.mark(&resource.id);
    let agent = desk.activities.agent_name(&resource.id).unwrap_or_default();
    let current = desk
        .shown
        .as_ref()
        .is_some_and(|s| s.resource == resource.id);
    let detail = [
        resource.session.as_str(),
        agent,
        mark.word(),
        if current { "current" } else { "" },
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let (id, session) = (resource.id.clone(), resource.session.clone());
    let label = format!("{} · {detail}", resource.label);
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
    .fill(if current {
        p.current
    } else {
        rune::Color::TRANSPARENT
    })
    .children([
        el().size(7.0, 7.0)
            .radius(3.5)
            .fill(sidebar::mark_color(mark, p)),
        col()
            .grow(1.0)
            .min_w(0.0)
            .pl(8.0)
            .py(6.0)
            .child(
                text(resource.label.clone())
                    .font(CAP_WORD)
                    .color(p.text)
                    .truncate(),
            )
            .child(text(detail).font(SUBTITLE).color(p.dim).truncate()),
    ])
}

fn other_hosts(mut list: El, desk: &Desk, query: &str, p: &Ink) -> El {
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

fn actions(desk: &Desk, p: &Ink) -> El {
    let mut list = col().w_full().padding(12.0).gap(4.0);
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
        list = list.child(if needs_pane && desk.shown.is_none() {
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
