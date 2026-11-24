//! The sidebar: one list of the work on the server. Each session's rows are
//! its tabs, then any terminal no tab holds: one line each, led by a tile
//! for who works there and how it goes, named after the task. What needs
//! you is counted per session. With the sidebar docked these rows are the
//! tabs, and the title bar has none (`tab-bar = "auto"`). Sessions
//! of one project sit together under its name, a linked worktree marked
//! `└`. They sit under the machine this window is on; the other machines
//! follow, folded, each opening to a glance at its sessions.
//!
//! Rows are phux's own (OWN), drawn only from the window's tokens:
//! the kit's `SidebarRow` has no disclosure on a draggable row, no status
//! words and no variable pitch. They sit in one source list, as the kit's
//! `Sidebar` keeps them: a single Tab stop whose arrows move the selection.

use crate::ui::tokens::Voice;
use std::collections::HashMap;

use phux_client_runtime::control::{SessionDescriptor, Topology};
use phux_protocol::ids::SessionKey;
use rune::{Color, El, Key, col, memo, row, text};

use super::controls::return_to_terminal_on_click;
use super::rows::State as Forced;
use super::{Paint, Tokens, icons, pressable, reveal, tile};
use crate::addressable::Addressable;
use crate::desk::{Desk, Grab};
use crate::host_color;
use crate::keys::Shortcut;

mod actions;
mod items;
use items::{Item, fold, given_name, items};
mod row;
use row::{Row, pane_row};
mod chrome;
use chrome::{awake_button, foot, inbox_button, search};
mod drag;
mod hosts;
use hosts::other_machines;
mod projects;
use projects::branch_mark;
mod marks;
mod satellites;

pub(super) use marks::{Counts, dot_size, face, histogram, mark_color, state_of};
use satellites::satellite_list;

/// What a session header draws. Everything its handlers capture is here, so
/// a memoized header is never stale.
#[derive(Hash)]
struct Header<'a> {
    id: u32,
    name: &'a str,
    key: Option<SessionKey>,
    current: bool,
    counts: Counts,
    count: usize,
    collapsed: bool,
    /// A satellite session's machine, and its colour so a recolour redraws.
    host: Option<(&'a str, u32)>,
    /// The session's `[[sessions]]` colour.
    color: Option<u32>,
    /// A linked worktree, marked under its project's primary checkout.
    branch: bool,
    /// A dragged session would land here.
    target: bool,
}

/// The header's disclosure: pointer only. The list's arrows press what they
/// reach, so a Tab stop here would fold the session as focus passed;
/// Left and Right on the header fold it instead.
fn disclosure(session: &Header, key: SessionKey, t: &Tokens) -> El {
    let collapsed = session.collapsed;
    pressable(format!("disclose-session-{}", session.id), t)
        .size(t.icon.grid, t.space.s6)
        .center()
        .radius(t.radius.xs)
        .focusable(false)
        .expanded(!collapsed)
        .label(format!(
            "{} session {}",
            if collapsed { "Expand" } else { "Collapse" },
            session.name
        ))
        .help(if collapsed {
            "Show resources"
        } else {
            "Hide resources"
        })
        .on_press(move |d: &mut Desk| {
            if !d.sidebar_collapsed.remove(&key) {
                d.sidebar_collapsed.insert(key);
            }
        })
        .child(
            rune::icon(
                if collapsed {
                    &icons::CHEVRON_RIGHT
                } else {
                    &icons::CHEVRON
                },
                t.icon.size.sm,
            )
            .color(t.roles.text.dim.paint())
            .a11y_hidden(),
        )
}

fn session_header(session: &Header, t: &Tokens) -> El {
    let key = session.key;
    let label = session.name.to_owned();
    let name = label.clone();
    let sidebar = t.roles.layer.sidebar.paint();
    let ink = if session.current {
        t.roles.text.text
    } else {
        t.roles.text.dim
    };
    let heading = row()
        .key(format!("select-session-{}", session.id))
        .grow(1.0)
        .min_w(0.0)
        .h(t.space.row_h)
        .gap(t.space.icon_gap)
        .radius(t.radius.md)
        .items_center()
        .children((session.host).map(|(host, _)| host_color::dot(host, dot_size(t), sidebar)))
        .children((session.color).map(|rgb| host_color::mark(rgb, dot_size(t), sidebar)))
        .children(session.branch.then(|| branch_mark(t)))
        .child(
            // A group's name, a step quieter than the rows it holds.
            text(session.name.to_owned())
                .font(t.ty.caption.semibold())
                .color(ink.paint())
                .truncate(),
        );
    let heading = match key {
        Some(key) => {
            let collapsed = session.collapsed;
            t.kit
                .interactive(heading)
                .focusable(true)
                .focus_selected(session.current)
                .expanded(!collapsed)
                .cursor(rune::Cursor::Grab)
                .label(format!("Open session {label}"))
                .help("Drag to reorder session")
                .on_press_tx(move |d: &mut Desk, tx| {
                    d.show_session(&name);
                    return_to_terminal_on_click(tx);
                })
                .on_key(move |d: &mut Desk, event, _| {
                    // A tree's Left folds and Right unfolds.
                    let fold = match event.key {
                        Key::Left => true,
                        Key::Right => false,
                        _ => return false,
                    };
                    if fold == collapsed {
                        return false;
                    }
                    if fold {
                        d.sidebar_collapsed.insert(key);
                    } else {
                        d.sidebar_collapsed.remove(&key);
                    }
                    true
                })
                .on_drag(move |d: &mut Desk, drag, _| d.drag_reorder(Grab::Session(key), drag))
                .on_double_press(move |d: &mut Desk| d.rename_session(key, &label))
        }
        None => heading.label(session.name.to_owned()),
    };
    let header = row()
        .key(format!("session-{}", session.id))
        .w_full()
        .h(t.space.row_h)
        .px(t.space.icon_gap)
        .gap(t.space.s1)
        .radius(t.radius.md)
        .items_center()
        .hover_group()
        .children(key.map(|key| disclosure(session, key, t)))
        .child(heading)
        // Open, its rows are the count; folded, the count stands for them.
        .children(session.collapsed.then(|| {
            text(session.count.to_string())
                .font(t.ty.caption)
                .color(t.roles.text.faint.paint())
                .a11y_hidden()
        }))
        .child(histogram(session.counts, t))
        // Close waits under the pointer: one on every header is a lot of
        // ink beside the names.
        .child(reveal(actions::session_controls(key, session.name, t), true));
    drag::reorderable(header, key, session.target, t)
}

/// Marks that need you, by session: one pass, for every header and the button.
fn counts_by_session(desk: &Desk, topology: &Topology) -> HashMap<u32, Counts> {
    let mut counts: HashMap<u32, Counts> = HashMap::new();
    for pane in &topology.panes {
        let mark = desk.activities.mark(&pane.terminal_id);
        if mark.needs_you() {
            counts.entry(pane.session_id).or_default().add(mark);
        }
    }
    counts
}

/// The rows' source list (the kit `Sidebar`'s): one Tab stop that lands on
/// the selected row; Up and Down move the selection, which follows focus,
/// typing a row's first letters jumps to it, and Esc leaves.
fn source_list(t: &Tokens) -> El {
    col()
        .key("sidebar-list")
        .role("list")
        .label("Sessions")
        .focus_group()
        .focus_follows()
        // Esc hands the keyboard back to the shown terminal.
        .on_key(|_: &mut Desk, event, tx| {
            let back = event.key == Key::Escape;
            if back {
                tx.blur();
            }
            back
        })
        .w_full()
        .gap(t.space.s0_5)
        .px(t.space.s2)
}

/// Every session's header and rows (its terminals, then its documents),
/// rebuilt only where their inputs moved: the view runs for each frame of
/// terminal output. A project's sessions follow its header, indented.
fn session_list(desk: &Desk, counts: &HashMap<u32, Counts>, t: &Tokens) -> El {
    let shown = desk.connection.shown.as_ref().map(|s| &s.resource);
    let current = desk.selected_session_name();
    // Everything a row reads from the tokens: a theme swapped under the
    // same id (Omarchy) or a contrast change still repaints.
    let version = t.version;
    let all = desk.addressable();
    // One pass, not one scan of every resource per session.
    let mut by_session: HashMap<u32, Vec<&Addressable>> = HashMap::new();
    for resource in &all {
        by_session
            .entry(resource.session_id)
            .or_default()
            .push(resource);
    }
    let here = desk.machines.label();
    let host = (here, host_color::entry(here).0);
    let counts_in = |id: u32| counts.get(&id).copied().unwrap_or_default();
    let dropping = desk.connection.reorder.target();
    let session_rows = |session: &SessionDescriptor, branch: bool| {
        let target = dropping.is_some() && dropping == session.key.map(Grab::Session);
        let mut out = Vec::new();
        let rows = by_session.get(&session.id).map_or(&[][..], Vec::as_slice);
        let tabs = desk.connection.tabs.by_session.get(&session.id);
        let items = items(tabs.map_or(&[][..], Vec::as_slice), rows);
        let color = crate::session_color::of(&session.name);
        let header = Header {
            id: session.id,
            name: &session.name,
            key: session.key,
            current: current.as_deref() == Some(session.name.as_str()),
            counts: counts_in(session.id),
            count: items.len(),
            collapsed: session
                .key
                .is_some_and(|key| desk.sidebar_collapsed.contains(&key)),
            host: None,
            color,
            branch,
            target,
        };
        out.push(memo(
            format!("session-{}", session.id),
            (&header, version),
            || session_header(&header, t),
        ));
        if header.collapsed {
            return out;
        }
        let titled: Vec<(&Item, &Addressable, String)> = items
            .iter()
            .map(|item| {
                let lead = item.lead(|id| desk.activities.agent_name(id).is_some());
                let named = item.tab.and_then(|tab| given_name(&tab.name));
                (item, lead, named.unwrap_or_else(|| lead.label.clone()))
            })
            .collect();
        let row_for = |index: usize, children: usize, open: bool, child: bool| {
            let (item, lead, name) = &titled[index];
            let id = &lead.id;
            let twin = titled.iter().filter(|(.., other)| other == name).count() > 1;
            let mirror = (!lead.kind.is_terminal()).then(|| lead.mirror.to_owned());
            let target = item.tab.and_then(|tab| desk.tab_focus(tab)).unwrap_or(id);
            let row = Row {
                id,
                target,
                session: &lead.session,
                name: name.clone(),
                place: twin.then(|| id.to_string()).or(mirror),
                key: session.key,
                agent: desk.activities.agent_name(id),
                kind: desk.activities.agent_kind(id),
                mark: tile::loudest(item.members.iter().map(|m| desk.activities.mark(&m.id)))
                    .unwrap_or_else(|| desk.activities.mark(id)),
                panes: item.members.len(),
                selected: item.members.iter().any(|m| shown == Some(&m.id)),
                children,
                open,
                child,
                host,
                color,
                state: Forced::Rest,
            };
            memo(format!("pane-{id}"), (&row, version), || pane_row(&row, t))
        };
        let selected =
            |index: usize| (titled[index].0.members.iter()).any(|m| shown == Some(&m.id));
        // Child panes fold under the pane they were started from: shut
        // until unfolded, but never over the pane on screen.
        for (root, kids) in fold(&items) {
            let open = desk.sidebar_unfolded.contains(&titled[root].1.id)
                || kids.iter().any(|&kid| selected(kid));
            out.push(row_for(root, kids.len(), open, false));
            if open {
                out.extend(kids.into_iter().map(|kid| row_for(kid, 0, false, true)));
            }
        }
        out
    };
    projects::grouped(
        source_list(t),
        &desk.ordered_sessions(),
        |id| counts_in(id).waiting(),
        version,
        t,
        session_rows,
    )
}

pub fn view(desk: &Desk, ink: &Tokens, inset: f32, leading: f32) -> El {
    let t = ink;
    let s = &t.space;
    let topology = desk.connection.topology.as_ref();
    let counts = topology.map_or_else(HashMap::new, |top| counts_by_session(desk, top));
    let list = topology.map_or_else(col, |top| {
        col()
            .w_full()
            .child(session_list(desk, &counts, t))
            .children((!top.satellite_sessions.is_empty()).then(|| satellite_list(desk, top, t)))
    });
    let waiting = desk.inbox.approvals.len() + counts.values().map(|c| c.waiting()).sum::<usize>();
    col()
        .w(s.sidebar_w)
        .h_full()
        // With glass on, the window's tinted glass shows through instead.
        .fill(if t.glass_tint_alpha(desk.glass()) < 1.0 {
            Color::TRANSPARENT
        } else {
            t.roles.layer.sidebar.paint()
        })
        .child(
            row()
                .key("sidebar-titlebar")
                .w_full()
                .h(inset)
                .pl(leading + s.s2)
                .pr(s.s2)
                .gap(s.s1)
                .items_center()
                .window_drag()
                .child(super::machines::switch(desk, ink).grow(1.0))
                .child(search(t))
                .children(awake_button(desk, t))
                .child(inbox_button(waiting, t)),
        )
        .child(
            drag::region(col()
                .key("sidebar-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(list)
                .child(other_machines(desk, t))),
        )
        .child(
            row()
                .key("sidebar-footer")
                .w_full()
                .px(s.s2)
                .py(s.icon_gap)
                .gap(s.s0_5)
                .child(foot(
                    "Terminal",
                    "new-terminal",
                    Shortcut::NewTerminal,
                    t,
                    Desk::new_terminal,
                ))
                .child(foot(
                    "Session",
                    "new-session",
                    Shortcut::NewSession,
                    t,
                    Desk::new_session,
                )),
        )
}

#[cfg(test)]
pub(in crate::ui) mod tests;
