//! The sidebar: every session and terminal on the server, each terminal
//! with what it is doing, and what needs you counted per session. Sessions
//! of one project sit together under its name, a linked worktree marked
//! `└`. They sit under the machine this window is on; the other machines
//! follow, folded, each opening to a glance at its sessions.

use std::collections::HashMap;

use phux_client_runtime::control::{SessionDescriptor, Topology};
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Color, El, col, el, memo, row, text};

use super::{CAPTION, Ink, LABEL, NAME, SIDEBAR_W, icons, keycap, lifted, pressable};
use crate::activity::Mark;
use crate::desk::Desk;
use crate::host_color;
use crate::keys::{self, Shortcut};
use crate::palette::pane_label;

mod hosts;
use hosts::{host_header, other_machines};
mod projects;
use projects::branch_mark;

/// A mark's colour, from the theme's own palette.
pub(super) fn mark_color(mark: Mark, ink: &Ink) -> Color {
    match mark {
        Mark::Blocked | Mark::Failed => ink.marks.alert,
        Mark::Done => ink.marks.done,
        Mark::Bell => ink.marks.bell,
        Mark::Working => ink.marks.working,
        Mark::Running => ink.accent,
        Mark::Background => ink.accent.alpha(0.55),
        Mark::Succeeded | Mark::Idle => ink.marks.good,
        Mark::None => ink.separator,
    }
}

/// A row's mark: a state that asks or reports in the theme's colour, a
/// terminal merely running or resting in its machine's.
fn row_color(mark: Mark, ink: &Ink, host: Color) -> Color {
    match mark {
        Mark::Running | Mark::Succeeded | Mark::Idle => host,
        Mark::Background => host.alpha(0.55),
        _ => mark_color(mark, ink),
    }
}

/// A ring while work is under way, a solid dot otherwise; never animated,
/// so a busy agent costs no frames. A plain shell gets a faint speck, so
/// names line up without a column of loud nothing.
fn dot(mark: Mark, color: Color, ink: &Ink) -> El {
    let dot = match mark {
        Mark::None => el().size(4.0, 4.0).radius(2.0).fill(ink.dim.alpha(0.45)),
        _ if mark.live() || mark == Mark::Background => {
            el().size(8.0, 8.0).radius(4.0).border(1.5, color)
        }
        _ => el().size(8.0, 8.0).radius(4.0).fill(color),
    };
    el().w(14.0).h(14.0).center().child(dot)
}

fn search(ink: &Ink) -> El {
    // ⌘K is the chord this field has always taught; ⌘P reaches it too.
    let chord: &[&str] = if cfg!(target_os = "macos") {
        &["⌘", "K"]
    } else {
        &["Ctrl", "Shift", "K"]
    };
    pressable("sidebar-search", ink)
        .w_full()
        .h(28.0)
        .px(8.0)
        .gap(7.0)
        .radius(7.0)
        .items_center()
        .fill(ink.text.alpha(0.05))
        .border_gradient(1.0, ink.text.alpha(0.09), ink.text.alpha(0.04))
        .on_press(|d: &mut Desk| d.toggle_palette())
        .child(
            rune::icon(&icons::SEARCH, 14.0)
                .color(ink.dim)
                .a11y_hidden(),
        )
        .child(text("Jump to…").font(LABEL).color(ink.dim))
        .child(el().grow(1.0))
        .child(
            row()
                .gap(2.0)
                .children(chord.iter().map(|key| keycap(*key, ink))),
        )
}

/// The way into the inbox, loud only when something waits.
fn inbox_button(waiting: usize, ink: &Ink) -> El {
    if waiting == 0 {
        return super::icon_press(
            &icons::INBOX,
            "Inbox",
            "inbox-button",
            ink,
            |d: &mut Desk| {
                d.toggle_inbox();
            },
        );
    }
    let tint = mark_color(Mark::Blocked, ink);
    pressable("inbox-button", ink)
        .h(22.0)
        .px(8.0)
        .gap(5.0)
        .radius(11.0)
        .items_center()
        .fill(tint.alpha(0.18))
        .label(format!("Inbox, {waiting} waiting"))
        .on_press(|d: &mut Desk| d.toggle_inbox())
        .child(
            text(format!("{waiting} waiting"))
                .font(CAPTION)
                .color(tint)
                .a11y_hidden(),
        )
}

/// What a session header draws. Everything its handlers capture is here, so
/// a memoized header is never stale.
#[derive(Hash)]
struct Header<'a> {
    id: u32,
    name: &'a str,
    key: Option<SessionKey>,
    current: bool,
    waiting: usize,
    /// A satellite session's machine, and its colour so a recolour redraws.
    host: Option<(&'a str, u32)>,
    /// A linked worktree, marked under its project's primary checkout.
    branch: bool,
}

/// A pill counting what needs you.
fn waiting_badge(waiting: usize, ink: &Ink) -> El {
    el().px(6.0)
        .radius(8.0)
        .fill(mark_color(Mark::Blocked, ink).alpha(0.22))
        .child(
            text(waiting.to_string())
                .font(CAPTION)
                .color(mark_color(Mark::Blocked, ink)),
        )
}

fn session_header(session: &Header, ink: &Ink) -> El {
    let key = session.key;
    let label = session.name.to_owned();
    let header = row()
        .key(format!("session-{}", session.id))
        .w_full()
        .pt(10.0)
        .pb(3.0)
        .px(8.0)
        .gap(6.0)
        .items_center()
        .on_double_press(move |d: &mut Desk| {
            if let Some(key) = key {
                d.rename_session(key, &label);
            }
        })
        .children((session.host).map(|(host, _)| host_color::dot(host, 6.0, ink.sidebar)))
        .children(session.branch.then(|| branch_mark(ink)))
        .child(
            text(session.name.to_owned())
                .font(CAPTION)
                .color(if session.current { ink.text } else { ink.dim })
                .truncate(),
        )
        .child(el().grow(1.0));
    header.children((session.waiting > 0).then(|| waiting_badge(session.waiting, ink)))
}

/// What a terminal's row draws, likewise. A session of one terminal is one
/// row: `name` is the session, `place` the terminal, and `rename` the
/// double-press its missing header would have held.
#[derive(Hash)]
struct Row<'a> {
    id: &'a ResourceId,
    session: &'a str,
    name: String,
    place: Option<String>,
    rename: Option<SessionKey>,
    agent: Option<&'a str>,
    mark: Mark,
    selected: bool,
    /// The terminal's machine and its colour, so a recolour redraws.
    host: (&'a str, u32),
    /// A one-terminal session that is a linked worktree.
    branch: bool,
}

impl Row<'_> {
    /// The second line: what the terminal is doing, else where it is.
    fn detail(&self) -> Option<(String, bool)> {
        let word = (self.mark != Mark::None && self.mark != Mark::Idle).then(|| self.mark.word());
        match (self.agent, word) {
            (Some(agent), Some(word)) => Some((format!("{agent} · {word}"), true)),
            (None, Some(word)) => Some((word.to_owned(), true)),
            (Some(agent), None) => Some((agent.to_owned(), false)),
            (None, None) => self.place.clone().map(|place| (place, false)),
        }
    }
}

fn pane_row(pane: &Row, ink: &Ink) -> El {
    let (mark, selected) = (pane.mark, pane.selected);
    let (id, session) = (pane.id.clone(), pane.session.to_owned());
    let host = host_color::on(pane.host.0, ink.sidebar);
    let accent = row_color(mark, ink, host);
    let (key, label) = (pane.rename, pane.name.clone());
    let detail = pane.detail().map(|(words, tinted)| {
        text(words)
            .font(CAPTION)
            .color(if tinted { accent } else { ink.dim })
            .truncate()
    });
    let base = row().key(format!("pane-{}", pane.id));
    let base = if selected { lifted(base, ink) } else { base };
    base.w_full()
        .min_h(30.0)
        .px(6.0)
        .py(5.0)
        .gap(6.0)
        .radius(7.0)
        .continuous_corners()
        .items_center()
        .hover(ink.hover)
        .press(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(move |d: &mut Desk| d.show(id.clone(), &session))
        .on_double_press(move |d: &mut Desk| {
            if let Some(key) = key {
                d.rename_session(key, &label);
            }
        })
        .children(pane.branch.then(|| branch_mark(ink)))
        .child(dot(mark, accent, ink))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(1.0)
                .child(
                    text(pane.name.clone())
                        .font(NAME)
                        .color(if selected {
                            ink.text
                        } else {
                            ink.text.alpha(0.82)
                        })
                        .truncate(),
                )
                .children(detail),
        )
        .children(selected.then(|| host_color::stripe(host)))
}

/// Marks that need you, by session: one pass, for every header and the button.
fn waiting_by_session(desk: &Desk, topology: &Topology) -> HashMap<u32, usize> {
    let mut waiting = HashMap::new();
    for pane in &topology.panes {
        if desk.activities.mark(&pane.terminal_id).needs_you() {
            *waiting.entry(pane.session_id).or_default() += 1;
        }
    }
    waiting
}

/// Every session's header and rows, rebuilt only where their inputs moved:
/// the view runs for each frame of terminal output. A project's sessions
/// follow its header, indented.
fn session_list(desk: &Desk, topology: &Topology, waiting: &HashMap<u32, usize>, ink: &Ink) -> El {
    let list = col().w_full().gap(1.0).px(8.0);
    let shown = desk.shown.as_ref().map(|s| &s.terminal);
    let current = desk.selected_session_name();
    let theme = desk.settings.theme.map(|t| t.id);
    let here = desk.machines.label();
    let host = (here, host_color::entry(here).0);
    let waiting_in = |id: u32| waiting.get(&id).copied().unwrap_or(0);
    let session_rows = |session: &SessionDescriptor, branch: bool| {
        let mut out = Vec::new();
        let panes: Vec<_> = topology
            .panes
            .iter()
            .filter(|p| p.session_id == session.id)
            .collect();
        let alone = panes.len() == 1;
        let labels: Vec<_> = panes.iter().map(|p| pane_label(p)).collect();
        if !alone {
            let header = Header {
                id: session.id,
                name: &session.name,
                key: session.key,
                current: current.as_deref() == Some(session.name.as_str()),
                waiting: waiting_in(session.id),
                host: None,
                branch,
            };
            out.push(memo(
                format!("session-{}", session.id),
                (&header, theme),
                || session_header(&header, ink),
            ));
        }
        for (pane, label) in panes.iter().zip(&labels) {
            let id = &pane.terminal_id;
            let (name, place) = if alone {
                let place = (*label != session.name).then(|| label.clone());
                (session.name.clone(), place)
            } else {
                // Twins are told apart by what the CLI calls them.
                let twin = labels.iter().filter(|other| *other == label).count() > 1;
                (label.clone(), twin.then(|| id.to_string()))
            };
            let row = Row {
                id,
                session: &pane.session_name,
                name,
                place,
                rename: session.key.filter(|_| alone),
                agent: desk.activities.agent_name(id),
                mark: desk.activities.mark(id),
                selected: shown == Some(id),
                host,
                branch: branch && alone,
            };
            out.push(memo(format!("pane-{id}"), (&row, theme), || {
                pane_row(&row, ink)
            }));
        }
        out
    };
    projects::grouped(
        list,
        &topology.sessions,
        waiting_in,
        theme,
        ink,
        session_rows,
    )
}

/// Whether a satellite session member is a Terminal: not a document or an
/// agent session the hub's topology already knows.
fn is_terminal_member(topology: &Topology, id: &ResourceId) -> bool {
    !topology.documents.iter().any(|doc| &doc.id == id)
        && !topology.agent_sessions.iter().any(|agent| &agent.id == id)
}

/// A hub's satellite sessions, each under a `host · session` header: its
/// terminals, then the mirrors their producers run (#2051 keeps them out of
/// the hub's own sessions).
fn satellite_list(desk: &Desk, topology: &Topology, ink: &Ink) -> El {
    let mut list = col().w_full().gap(1.0).px(8.0);
    let shown = desk.shown.as_ref().map(|s| &s.terminal);
    let theme = desk.settings.theme.map(|t| t.id);
    for (n, session) in topology.satellite_sessions.iter().enumerate() {
        let name = format!("{} · {}", session.host, session.name);
        let host = (session.host.as_str(), host_color::entry(&session.host).0);
        let header = Header {
            id: u32::MAX - u32::try_from(n).unwrap_or(0),
            name: &name,
            key: None,
            current: session.members.iter().any(|id| shown == Some(id)),
            waiting: 0,
            host: Some(host),
            branch: false,
        };
        list = list.child(memo(format!("satellite-{name}"), (&header, theme), || {
            session_header(&header, ink)
        }));
        // Members are every kind the session names; documents are listed
        // once below as mirrors and agent sessions have no row of their own.
        let terminals = session
            .members
            .iter()
            .filter(|id| is_terminal_member(topology, id))
            .map(|id| {
                let label = topology.pane(id).map_or_else(|| id.to_string(), pane_label);
                (id, label, None)
            });
        let mirrors = topology
            .documents
            .iter()
            .filter(|doc| {
                session.members.contains(&doc.id)
                    || doc
                        .parent
                        .as_ref()
                        .is_some_and(|p| session.members.contains(p))
            })
            .map(|doc| {
                let label = doc.name.clone().unwrap_or_else(|| "Mirror".to_owned());
                (&doc.id, label, Some(desk.mirror_label(&doc.id).to_owned()))
            });
        for (id, label, place) in terminals.chain(mirrors) {
            let row = Row {
                id,
                session: "",
                name: label,
                place,
                rename: None,
                agent: desk.activities.agent_name(id),
                mark: desk.activities.mark(id),
                selected: shown == Some(id),
                host,
                branch: false,
            };
            list = list.child(memo(format!("pane-{id}"), (&row, theme), || {
                pane_row(&row, ink)
            }));
        }
    }
    list
}

/// A word to press at the foot of the sidebar. Under the pointer it shows
/// the chord that does the same, so the keys get learned where the mouse
/// already is.
fn foot(
    desk: &Desk,
    label: &'static str,
    key: &'static str,
    action: Shortcut,
    ink: &Ink,
    press: fn(&mut Desk),
) -> El {
    let hovered = desk.foot_hover == Some(key);
    let chord = keys::command(action).map(keys::Command::hint);
    pressable(key, ink)
        .h(26.0)
        .px(6.0)
        .gap(5.0)
        .radius(6.0)
        .items_center()
        .on_press(press)
        .on_hover_change(move |d: &mut Desk, inside, _| {
            if inside {
                d.foot_hover = Some(key);
            } else if d.foot_hover == Some(key) {
                d.foot_hover = None;
            }
        })
        .child(rune::icon(&icons::PLUS, 12.0).color(ink.host))
        .child(text(label).font(LABEL).color(ink.dim))
        .child(match chord.filter(|_| hovered) {
            Some(chord) => text(chord).font(CAPTION).color(ink.dim),
            None => text(""),
        })
}

pub fn view(desk: &Desk, ink: &Ink, inset: f32) -> El {
    let topology = desk.topology.as_ref();
    let counts = topology.map_or_else(HashMap::new, |t| waiting_by_session(desk, t));
    let folded = desk.machines.folded;
    let list = topology.filter(|_| !folded).map_or_else(col, |t| {
        col()
            .w_full()
            .child(session_list(desk, t, &counts, ink))
            .child(satellite_list(desk, t, ink))
    });
    let here = desk.machines.current.clone();
    let this_machine = host_header(
        desk.machines.label(),
        !folded,
        topology.map(|t| t.sessions.len().to_string()),
        ink,
        move |d: &mut Desk| d.toggle_host(here.clone()),
    );
    let waiting = desk.inbox.approvals.len() + counts.values().sum::<usize>();
    col()
        .w(SIDEBAR_W)
        .h_full()
        .fill(ink.sidebar)
        .child(el().w_full().h(inset).window_drag())
        .child(
            row()
                .w_full()
                .px(8.0)
                .pb(6.0)
                .gap(4.0)
                .items_center()
                .justify_between()
                .child(super::machines::switch(desk, ink))
                .child(inbox_button(waiting, ink)),
        )
        .child(el().w_full().px(8.0).pb(6.0).child(search(ink)))
        .child(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(el().w_full().px(8.0).child(this_machine))
                .child(list)
                .child(other_machines(desk, ink)),
        )
        .child(
            row()
                .w_full()
                .px(8.0)
                .py(6.0)
                .gap(2.0)
                .child(foot(
                    desk,
                    "Terminal",
                    "new-terminal",
                    Shortcut::NewTerminal,
                    ink,
                    Desk::new_terminal,
                ))
                .child(foot(
                    desk,
                    "Session",
                    "new-session",
                    Shortcut::NewSession,
                    ink,
                    Desk::new_session,
                )),
        )
}

#[cfg(test)]
mod tests {
    use crate::host_color;
    use crate::theme::{DIM_BAR, contrast};
    use phux_config_core::host_color::PALETTE;

    #[test]
    fn host_marks_read_in_every_theme() {
        let mut low = Vec::new();
        for theme in std::iter::once(None).chain(crate::theme::all().map(Some)) {
            for dark in [true, false] {
                let ink = super::super::ink(dark, rune::oklch(0.6, 0.15, 250.0), theme);
                for (i, (id, _)) in PALETTE.iter().enumerate() {
                    let host = host_color::swatch(i, ink.sidebar);
                    let name = theme.map_or("system", |t| t.id);
                    // Eased for the sidebar; the focused card's rim sits on
                    // the window ground, a shade away.
                    let mark = contrast(host, ink.sidebar);
                    if mark < DIM_BAR || contrast(host, ink.bg) < DIM_BAR - 0.1 {
                        low.push(format!("{id} on {name}/{dark}: {mark:.2}"));
                    }
                }
            }
        }
        assert!(low.is_empty(), "{low:#?}");
    }
}
