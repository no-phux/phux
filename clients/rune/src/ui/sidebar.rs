//! The sidebar: every session and terminal on the server, each terminal
//! with what it is doing, and what needs you counted per session.

use std::collections::HashMap;

use phux_client_runtime::control::Topology;
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Color, El, TextStyle, col, el, memo, row, text};

use super::{CAPTION, Ink, LABEL, LABEL_STRONG, SIDEBAR_W, button, pressable};
use crate::activity::Mark;
use crate::desk::Desk;
use crate::palette::pane_label;

/// A mark's colour: Catppuccin's, as the GPUI desktop uses them.
pub(super) fn mark_color(mark: Mark, ink: &Ink) -> Color {
    let rgb = |r, g, b| crate::grid::color(phux_client_runtime::publication::Rgb { r, g, b });
    match mark {
        Mark::Blocked | Mark::Failed => rgb(0xf3, 0x8b, 0xa8),
        Mark::Done => rgb(0x94, 0xe2, 0xd5),
        Mark::Bell => rgb(0xfa, 0xb3, 0x87),
        Mark::Working => rgb(0xf9, 0xe2, 0xaf),
        Mark::Running => ink.accent,
        Mark::Succeeded | Mark::Idle => rgb(0xa6, 0xe3, 0xa1),
        Mark::None => ink.separator,
    }
}

/// A ring while work is under way, a solid dot otherwise; never animated,
/// so a busy agent costs no frames.
fn dot(mark: Mark, ink: &Ink) -> El {
    let dot = el().size(7.0, 7.0).radius(3.5);
    let dot = if mark.live() {
        dot.border(1.5, mark_color(mark, ink))
    } else {
        dot.fill(mark_color(mark, ink))
    };
    el().w(14.0).h(14.0).center().child(dot)
}

fn search(ink: &Ink) -> El {
    pressable("sidebar-search", ink)
        .w_full()
        .h(28.0)
        .px(10.0)
        .gap(8.0)
        .radius(7.0)
        .items_center()
        .fill(ink.separator)
        .on_press(|d: &mut Desk| d.toggle_palette())
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
    let tint = if waiting > 0 {
        mark_color(Mark::Blocked, ink)
    } else {
        ink.dim
    };
    pressable("inbox-button", ink)
        .px(7.0)
        .py(2.0)
        .gap(5.0)
        .radius(7.0)
        .items_center()
        .fill(tint.alpha(if waiting > 0 { 0.2 } else { 0.1 }))
        .on_press(|d: &mut Desk| d.toggle_inbox())
        .child(
            text(if waiting > 0 {
                format!("{waiting} waiting")
            } else {
                "Inbox".to_owned()
            })
            .font(CAPTION)
            .color(tint),
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
}

fn session_header(session: &Header, ink: &Ink) -> El {
    let key = session.key;
    let label = session.name.to_owned();
    let header = row()
        .key(format!("session-{}", session.id))
        .w_full()
        .pt(12.0)
        .pb(4.0)
        .px(8.0)
        .gap(6.0)
        .items_center()
        .on_double_press(move |d: &mut Desk| {
            if let Some(key) = key {
                d.rename_session(key, &label);
            }
        })
        .child(
            text(session.name.to_uppercase())
                .font(TextStyle::new(10.5, 650.0).tracking(0.6))
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

/// What a terminal's row draws, likewise.
#[derive(Hash)]
struct Row<'a> {
    id: &'a ResourceId,
    session: &'a str,
    label: String,
    agent: Option<&'a str>,
    mark: Mark,
    selected: bool,
}

fn pane_row(pane: &Row, ink: &Ink) -> El {
    let (mark, selected) = (pane.mark, pane.selected);
    let (id, session) = (pane.id.clone(), pane.session.to_owned());
    let accent = mark_color(mark, ink);
    let mut line = row()
        .w_full()
        .gap(6.0)
        .items_center()
        .child(dot(mark, ink))
        .child(
            text(pane.label.clone())
                .font(if selected { LABEL_STRONG } else { LABEL })
                .color(ink.text)
                .truncate(),
        )
        .child(el().grow(1.0));
    if let Some(agent) = pane.agent {
        line = line.child(
            el().px(6.0)
                .radius(4.0)
                .fill(accent.alpha(0.16))
                .child(text(agent.to_owned()).font(CAPTION).color(accent)),
        );
    }
    line = line.child(text(pane.id.to_string()).font(CAPTION).color(ink.dim));
    let detail = (mark != Mark::None && mark != Mark::Idle).then(|| {
        row()
            .w_full()
            .pl(20.0)
            .child(text(mark.word()).font(CAPTION).color(accent))
    });
    col()
        .key(format!("pane-{}", pane.id))
        .w_full()
        .px(6.0)
        .py(5.0)
        .gap(1.0)
        .radius(7.0)
        .fill(if selected {
            ink.selected
        } else {
            Color::TRANSPARENT
        })
        .border(
            1.0,
            if mark.needs_you() && !selected {
                accent.alpha(0.45)
            } else {
                Color::TRANSPARENT
            },
        )
        .hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(move |d: &mut Desk| d.show(id.clone(), &session))
        .child(line)
        .children(detail)
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
    let mut list = col().w_full().gap(2.0).px(8.0);
    let shown = desk.shown.as_ref().map(|s| &s.terminal);
    let current = desk.selected_session_name();
    let theme = desk.settings.theme.map(|t| t.id);
    for session in &topology.sessions {
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
        for pane in topology.panes.iter().filter(|p| p.session_id == session.id) {
            let id = &pane.terminal_id;
            let row = Row {
                id,
                session: &pane.session_name,
                label: pane_label(pane),
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
                .px(14.0)
                .pb(8.0)
                .items_center()
                .justify_between()
                .child(
                    text("phux")
                        .font(TextStyle::new(13.0, 700.0))
                        .color(ink.text),
                )
                .child(inbox_button(waiting, ink)),
        )
        .child(super::machines::switch(desk, ink))
        .child(el().w_full().px(8.0).pb(4.0).child(search(ink)))
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
                .padding(8.0)
                .gap(6.0)
                .child(button(
                    "New terminal",
                    "new-terminal",
                    ink,
                    |d: &mut Desk| d.new_terminal(),
                ))
                .child(button("New session", "new-session", ink, |d: &mut Desk| {
                    d.new_session()
                })),
        )
}
