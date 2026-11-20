//! The sidebar: every session and terminal on the server, each terminal
//! with what it is doing, and what needs you counted per session. Sessions
//! of one project sit together under its name, a linked worktree marked
//! `└`. They sit under the machine this window is on; the other machines
//! follow, folded, each opening to a glance at its sessions.
//!
//! Rows are phux's own (§4.1.3 OWN), drawn only from the window's tokens:
//! the kit's `SidebarRow` has no disclosure on a draggable row, no status
//! words and no variable pitch. They sit in one source list, as the kit's
//! `Sidebar` keeps them: a single Tab stop whose arrows move the selection.

use crate::ui::tokens::Voice;
use std::collections::HashMap;

use phux_client_runtime::control::{SessionDescriptor, Topology};
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Color, El, Key, col, el, memo, row, text};

use super::rows::{Base, State as Forced, forced_face};
use super::{Paint, Tokens, icons, pressable};
use crate::activity::Mark;
use crate::desk::{Desk, Grab};
use crate::host_color;
use crate::keys::Shortcut;

mod actions;
mod chrome;
use chrome::{foot, inbox_button, search};
mod drag;
mod hosts;
use hosts::other_machines;
mod projects;
use projects::branch_mark;
mod marks;
mod satellites;
use marks::lead;
pub(super) use marks::{Counts, Face, dot_size, face, histogram, mark_color, state_of};
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
            text(session.name.to_owned())
                .font(t.ty.label.semibold())
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
                    clicked(tx);
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
        .children(key.map(|key| disclosure(session, key, t)))
        .child(heading)
        .child(
            text(session.count.to_string())
                .font(t.ty.caption)
                .color(t.roles.text.dim.paint())
                .a11y_hidden(),
        )
        .child(histogram(session.counts, t))
        .child(actions::session_controls(key, session.name, t));
    drag::reorderable(header, key, session.target, t)
}

/// A resource row, always below its session's separately actionable header.
#[derive(Hash)]
struct Row<'a> {
    id: &'a ResourceId,
    session: &'a str,
    name: String,
    place: Option<String>,
    key: Option<SessionKey>,
    agent: Option<&'a str>,
    mark: Mark,
    selected: bool,
    /// The terminal's machine and its colour, so a recolour redraws.
    host: (&'a str, u32),
    /// The session's colour, for its quiet mark.
    color: Option<u32>,
    /// A look forced for the gallery's states page; `Rest` is the live row.
    state: Forced,
}

impl Row<'_> {
    /// The second line: what the terminal is doing, in the status word
    /// after the agent's name, then where it is, so two rows with one name
    /// always read differently. Whether the word leads.
    fn detail(&self, t: &Tokens) -> Option<(String, bool)> {
        let word = (self.mark != Mark::None)
            .then(|| face(self.mark, t).word)
            .flatten();
        let doing = match (self.agent, word) {
            (Some(agent), Some(word)) => Some((format!("{agent} · {word}"), true)),
            (None, Some(word)) => Some((word.to_owned(), true)),
            (Some(agent), None) => Some((agent.to_owned(), false)),
            (None, None) => None,
        };
        match (doing, &self.place) {
            (Some((doing, tinted)), Some(place)) => Some((format!("{doing} · {place}"), tinted)),
            (doing, place) => doing.or_else(|| place.clone().map(|place| (place, false))),
        }
    }
}

fn pane_row(pane: &Row, t: &Tokens) -> El {
    let (mark, selected) = (pane.mark, pane.selected);
    let (id, session) = (pane.id.clone(), pane.session.to_owned());
    let host = host_color::on(pane.host.0, t.roles.layer.sidebar.paint());
    let key = pane.key;
    let state = pane.state;
    let disabled = state == Forced::Disabled;
    let washed = selected || matches!(state, Forced::Hover | Forced::Press);
    let detail = pane.detail(t).map(|(words, tinted)| {
        // Faint never sits on a wash: a lit row promotes it to dim.
        let quiet = if washed || disabled {
            t.roles.text.dim
        } else {
            t.roles.text.faint
        };
        let f = face(mark, t);
        let lit = tinted && !disabled;
        let ink = if lit { f.ink } else { quiet.paint() };
        // What asks for you is set bold beside its mark (§1.3).
        let font = if lit && f.bold {
            t.ty.caption.semibold()
        } else {
            t.ty.caption
        };
        text(words).font(font).color(ink).truncate().a11y_hidden()
    });
    let said = pane.detail(t).map(|(words, _)| words).unwrap_or_default();
    let name_ink = if disabled {
        t.roles.text.faint
    } else {
        t.roles.text.text
    };
    let under = if selected {
        Base::Selected
    } else {
        Base::Plain
    };
    let base = forced_face(row().key(format!("pane-{}", pane.id)), under, state, t);
    let base = if state == Forced::Rest {
        t.kit.interactive(base)
    } else {
        base
    };
    let row = base
        .w_full()
        .min_h(t.space.row_h)
        .pl(t.space.s4 + t.space.s0_5)
        .pr(t.space.icon_gap)
        .py(t.space.s0_5)
        .gap(t.space.icon_gap)
        .radius(t.radius.md)
        .continuous_corners()
        .items_center()
        .focusable(!disabled)
        .focus_selected(selected)
        .cursor(rune::Cursor::Pointer)
        .label(format!("{} in session {}", pane.name, pane.session))
        .description(said)
        .child(lead(mark, pane.color, t))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .child(
                    text(pane.name.clone())
                        .font(t.ty.label.medium())
                        .color(name_ink.paint())
                        .truncate()
                        .a11y_hidden(),
                )
                .children(detail),
        )
        .children(selected.then(|| host_color::stripe(host, t)));
    if disabled {
        return row;
    }
    let row = row.on_press_tx(move |d: &mut Desk, tx| {
        d.show(id.clone(), &session);
        clicked(tx);
    });
    match key {
        Some(key) => drag::resource(row, pane.id, key),
        None => row,
    }
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

/// A click on a row picks it and hands the keyboard back to the shown
/// terminal; only keys keep focus in the list, where arrows walk it.
fn clicked(tx: &mut rune::Tx) {
    if !tx.by_keys() {
        tx.blur();
    }
}

/// The rows' source list (the kit `Sidebar`'s): one Tab stop that lands on
/// the selected row; Up and Down move the selection, which follows focus,
/// and typing a row's first letters jumps to it.
fn source_list(t: &Tokens) -> El {
    col()
        .key("sidebar-list")
        .role("list")
        .label("Sessions")
        .focus_group()
        .focus_follows()
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
    let here = desk.machines.label();
    let host = (here, host_color::entry(here).0);
    let counts_in = |id: u32| counts.get(&id).copied().unwrap_or_default();
    let dropping = desk.connection.reorder.target();
    let session_rows = |session: &SessionDescriptor, branch: bool| {
        let target = dropping.is_some() && dropping == session.key.map(Grab::Session);
        let mut out = Vec::new();
        let rows: Vec<_> = all.iter().filter(|r| r.session_id == session.id).collect();
        let color = crate::session_color::of(&session.name);
        let header = Header {
            id: session.id,
            name: &session.name,
            key: session.key,
            current: current.as_deref() == Some(session.name.as_str()),
            counts: counts_in(session.id),
            count: rows.len(),
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
        for resource in &rows {
            let (id, label) = (&resource.id, &resource.label);
            let twin = rows.iter().filter(|other| other.label == *label).count() > 1;
            let mirror = (!resource.kind.is_terminal()).then(|| resource.mirror.to_owned());
            let (name, place) = (label.clone(), twin.then(|| id.to_string()).or(mirror));
            let row = Row {
                id,
                session: &resource.session,
                name,
                place,
                key: session.key,
                agent: desk.activities.agent_name(id),
                mark: desk.activities.mark(id),
                selected: shown == Some(id),
                host,
                color,
                state: Forced::Rest,
            };
            out.push(memo(format!("pane-{id}"), (&row, version), || {
                pane_row(&row, t)
            }));
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
                .child(inbox_button(waiting, t)),
        )
        .child(el().w_full().px(s.s2).pb(s.s2).child(search(t)))
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
