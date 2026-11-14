//! Touch-first session browsing and contextual actions, separate from search
//! commands. Keep chrome fixed while only the inventory scrolls.

use rune::{El, Key, TextStyle, Tx, col, el, row, text, text_input};

use super::{CAP_WORD, Desk, Ink, ROUND, SUBTITLE, TITLE};
use crate::addressable::Addressable;
use crate::desk::phone::navigation::Page;
use crate::keys::{self, Shortcut};
use crate::ui::{icons, pressable};

mod resource;
use resource::resource_row;

// One inset and rhythm for the page, cards and their touch rows. Colors and
// type come from the phone's existing theme, never a parallel palette.
const MARGIN: f32 = 16.0;
const GAP: f32 = 12.0;
const RADIUS: f32 = 12.0;
const INSET: f32 = 4.0;
const ROW_HEIGHT: f32 = 60.0;
const COLUMN_MAX: f32 = 600.0;
const PAGE_TITLE: TextStyle = TextStyle::new(24.0, 600.0);
const SESSION_TITLE: TextStyle = TextStyle::new(15.0, 600.0);

pub(super) fn view(desk: &Desk, p: &Ink, top: f32) -> Option<El> {
    let page = desk.phone_page()?;
    let title = match page {
        Page::Sessions => "Sessions",
        Page::Actions => "Terminal actions",
    };
    let header = row()
        .w_full()
        .max_w(COLUMN_MAX)
        .h(64.0)
        .pl(MARGIN)
        .pr(INSET)
        .gap(8.0)
        .items_center()
        .child(
            text(title)
                .font(if page == Page::Sessions {
                    PAGE_TITLE
                } else {
                    TITLE
                })
                .color(p.text),
        )
        .child(el().grow(1.0))
        .child(
            control("phone-navigation-close", "Done", p, |d, _| {
                d.close_phone_navigation()
            })
            .child(text("Done").font(CAP_WORD).color(p.accent).a11y_hidden()),
        );
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
            .items_center()
            .pt(top)
            .fill(p.bg)
            .modal(true, Desk::close_phone_navigation)
            .on_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
            .on_paste(|_: &mut Desk, _, _| {})
            .label(title)
            .child(header)
            .child(body.max_w(COLUMN_MAX)),
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
        .radius(RADIUS)
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
            .truncate()
            .a11y_hidden(),
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
        .grow(1.0)
        .min_w(0.0)
        .h(ROUND)
        .py(12.0)
        .label("Search sessions, panes, agents, and folders")
        .color(p.text)
        .placeholder("Search sessions or panes", p.dim)
        .on_edit(|d: &mut Desk| &mut d.phone.as_mut().expect("phone").query)
        .on_input_change(Desk::phone_search_edited)
        .on_input_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
        .on_submit(|d: &mut Desk, tx| d.blur_phone_search(tx));
    let search = row()
        .w_full()
        .px(GAP)
        .gap(8.0)
        .items_center()
        .radius(RADIUS)
        .fill(p.raised)
        .child(rune::icon(&icons::SEARCH, 16.0).color(p.dim).a11y_hidden())
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
    .gap(8.0)
    .center()
    .radius(ROUND / 2.0)
    .child(rune::icon(&icons::PLUS, 18.0).color(p.text).a11y_hidden())
    .child(
        text(if name.is_empty() {
            "New session"
        } else {
            "Open or create session"
        })
        .key("phone-session-new-label")
        .font(CAP_WORD)
        .color(p.text)
        .truncate()
        .a11y_hidden(),
    );
    let footer = super::glass(
        footer
            .backdrop_blur(crate::ui::FROST)
            .fill(p.bg.alpha(0.82)),
        p,
    );
    let footer = if desk.client.is_some() {
        footer
    } else {
        footer.inert()
    };
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .px(MARGIN)
        .gap(GAP)
        .pb(GAP)
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
                        .child(list.pb(ROUND + GAP)),
                )
                .child(footer.pin(Some(0.0), None, Some(0.0), Some(0.0))),
        )
}

fn connections(desk: &Desk, p: &Ink) -> El {
    let label = format!(
        "Connections · {} · {}",
        desk.machines.label(),
        desk.link_word()
    );
    control("phone-session-connections", &label, p, |d, tx| {
        d.phone_action(Shortcut::Machines, tx)
    })
    .w_full()
    .gap(8.0)
    .child(
        text(desk.machines.label().to_owned())
            .font(SUBTITLE)
            .color(p.dim)
            .truncate()
            .a11y_hidden(),
    )
    .child(el().grow(1.0))
    .child(
        text(desk.link_word())
            .font(SUBTITLE)
            .color(p.dim)
            .a11y_hidden(),
    )
    .child(
        rune::icon(&icons::CHEVRON_RIGHT, 14.0)
            .color(p.dim)
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
    let mut list = col().w_full().gap(GAP);
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

fn empty_state(desk: &Desk, query: &str, p: &Ink) -> El {
    let (title, detail) = if desk.topology.is_none() {
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
        .padding(GAP)
        .gap(8.0)
        .child(text(title).font(TITLE).color(p.text))
        .child(text(detail).font(SUBTITLE).color(p.dim).wrap())
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
    let name = session.name.clone();
    let current = desk.selected_session_name().as_deref() == Some(&session.name);
    let attention = if waiting == 1 {
        "1 needs you".to_owned()
    } else {
        format!("{waiting} need you")
    };
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
    .min_h(ROW_HEIGHT)
    .gap(8.0)
    .child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(INSET)
            .child(
                row()
                    .min_w(0.0)
                    .gap(6.0)
                    .items_center()
                    .child(
                        text(session.name.clone())
                            .font(SESSION_TITLE)
                            .color(p.text)
                            .truncate()
                            .a11y_hidden(),
                    )
                    .children(
                        current.then(|| rune::icon(&icons::CHECK, 14.0).color(p.dim).a11y_hidden()),
                    ),
            )
            .child(text(panes).font(SUBTITLE).color(p.dim).a11y_hidden()),
    )
    .children((waiting > 0).then(|| {
        el().px(8.0)
            .py(INSET)
            .radius(8.0)
            .fill(p.marks.alert.alpha(0.12))
            .child(
                text(attention)
                    .font(SUBTITLE)
                    .color(p.marks.alert)
                    .a11y_hidden(),
            )
    }));
    let mut card = col()
        .w_full()
        .padding(INSET)
        .radius(RADIUS + INSET)
        .fill(p.raised)
        .child(header);
    for resource in filtered {
        card = card.child(resource_row(desk, resource, p));
    }
    card
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
