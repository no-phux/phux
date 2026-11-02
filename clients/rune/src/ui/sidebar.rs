//! The sidebar: every session and terminal on the server, each terminal
//! with what it is doing, and what needs you counted per session.

use std::collections::HashMap;

use phux_client_runtime::control::Topology;
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Color, El, col, el, memo, row, text};

use super::{CAPTION, Ink, LABEL, NAME, SIDEBAR_W, icons, pressable};
use crate::activity::Mark;
use crate::desk::Desk;
use crate::palette::pane_label;

/// A mark's colour, from the theme's own palette.
pub(super) fn mark_color(mark: Mark, ink: &Ink) -> Color {
    match mark {
        Mark::Blocked | Mark::Failed => ink.marks.alert,
        Mark::Done => ink.marks.done,
        Mark::Bell => ink.marks.bell,
        Mark::Working => ink.marks.working,
        Mark::Running => ink.accent,
        Mark::Succeeded | Mark::Idle => ink.marks.good,
        Mark::None => ink.separator,
    }
}

/// A ring while work is under way, a solid dot otherwise; never animated,
/// so a busy agent costs no frames. A plain shell gets a faint speck, so
/// names line up without a column of loud nothing.
fn dot(mark: Mark, ink: &Ink) -> El {
    let dot = match mark {
        Mark::None => el().size(4.0, 4.0).radius(2.0).fill(ink.dim.alpha(0.45)),
        _ if mark.live() => el()
            .size(8.0, 8.0)
            .radius(4.0)
            .border(1.5, mark_color(mark, ink)),
        _ => el().size(8.0, 8.0).radius(4.0).fill(mark_color(mark, ink)),
    };
    el().w(14.0).h(14.0).center().child(dot)
}

fn search(ink: &Ink) -> El {
    pressable("sidebar-search", ink)
        .w_full()
        .h(28.0)
        .px(8.0)
        .gap(7.0)
        .radius(7.0)
        .items_center()
        .fill(ink.current.alpha(0.06))
        .on_press(|d: &mut Desk| d.toggle_palette())
        .child(rune::icon(&icons::SEARCH, 13.0).color(ink.dim))
        .child(text("Jump to…").font(LABEL).color(ink.dim))
        .child(el().grow(1.0))
        .child(
            text(if cfg!(target_os = "macos") {
                "⌘K"
            } else {
                "Ctrl+Shift+K"
            })
            .font(CAPTION)
            .color(ink.dim),
        )
}

/// The way into the inbox, loud only when something waits.
fn inbox_button(waiting: usize, ink: &Ink) -> El {
    if waiting == 0 {
        return super::icon_press(&icons::INBOX, "inbox-button", ink, |d: &mut Desk| {
            d.toggle_inbox();
        });
    }
    let tint = mark_color(Mark::Blocked, ink);
    pressable("inbox-button", ink)
        .h(22.0)
        .px(8.0)
        .gap(5.0)
        .radius(11.0)
        .items_center()
        .fill(tint.alpha(0.18))
        .on_press(|d: &mut Desk| d.toggle_inbox())
        .child(text(format!("{waiting} waiting")).font(CAPTION).color(tint))
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
        .child(
            text(session.name.to_owned())
                .font(CAPTION)
                .color(if session.current { ink.text } else { ink.dim })
                .truncate(),
        )
        .child(el().grow(1.0));
    if session.waiting == 0 {
        return header;
    }
    header.child(
        el().px(6.0)
            .radius(8.0)
            .fill(mark_color(Mark::Blocked, ink).alpha(0.22))
            .child(
                text(session.waiting.to_string())
                    .font(CAPTION)
                    .color(mark_color(Mark::Blocked, ink)),
            ),
    )
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
    let accent = mark_color(mark, ink);
    let (key, label) = (pane.rename, pane.name.clone());
    let detail = pane.detail().map(|(words, tinted)| {
        text(words)
            .font(CAPTION)
            .color(if tinted { accent } else { ink.dim })
            .truncate()
    });
    row()
        .key(format!("pane-{}", pane.id))
        .w_full()
        .min_h(30.0)
        .px(6.0)
        .py(5.0)
        .gap(6.0)
        .radius(7.0)
        .items_center()
        .fill(if selected {
            ink.current
        } else {
            Color::TRANSPARENT
        })
        .hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(move |d: &mut Desk| d.show(id.clone(), &session))
        .on_double_press(move |d: &mut Desk| {
            if let Some(key) = key {
                d.rename_session(key, &label);
            }
        })
        .child(dot(mark, ink))
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
/// the view runs for each frame of terminal output.
fn session_list(desk: &Desk, topology: &Topology, waiting: &HashMap<u32, usize>, ink: &Ink) -> El {
    let mut list = col().w_full().gap(1.0).px(8.0);
    let shown = desk.shown.as_ref().map(|s| &s.terminal);
    let current = desk.selected_session_name();
    let theme = desk.settings.theme.map(|t| t.id);
    for session in &topology.sessions {
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
                waiting: waiting.get(&session.id).copied().unwrap_or(0),
            };
            list = list.child(memo(
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
            };
            list = list.child(memo(format!("pane-{id}"), (&row, theme), || {
                pane_row(&row, ink)
            }));
        }
    }
    list
}

/// A word to press at the foot of the sidebar.
fn foot(label: &'static str, key: &'static str, ink: &Ink, press: fn(&mut Desk)) -> El {
    pressable(key, ink)
        .h(26.0)
        .px(6.0)
        .gap(5.0)
        .radius(6.0)
        .items_center()
        .on_press(press)
        .child(rune::icon(&icons::PLUS, 12.0).color(ink.dim))
        .child(text(label).font(LABEL).color(ink.dim))
}

pub fn view(desk: &Desk, ink: &Ink, inset: f32) -> El {
    let topology = desk.topology.as_ref();
    let counts = topology.map_or_else(HashMap::new, |t| waiting_by_session(desk, t));
    let list = topology.map_or_else(col, |t| session_list(desk, t, &counts, ink));
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
                .child(list),
        )
        .child(
            row()
                .w_full()
                .px(8.0)
                .py(6.0)
                .gap(2.0)
                .child(foot("Terminal", "new-terminal", ink, Desk::new_terminal))
                .child(foot("Session", "new-session", ink, Desk::new_session)),
        )
}
