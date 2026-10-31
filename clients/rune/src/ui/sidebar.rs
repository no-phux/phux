//! The sidebar: every session and terminal on the server, each terminal
//! with what it is doing, and what needs you counted per session.

use rune::{Color, El, LoopSpec, TextStyle, col, el, row, text};

use super::{CAPTION, Ink, LABEL, LABEL_STRONG, SIDEBAR_W, button, pressable};
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

fn dot(mark: Mark, ink: &Ink) -> El {
    let dot = el().size(7.0, 7.0).radius(3.5).fill(mark_color(mark, ink));
    let dot = if mark.live() {
        dot.looping(LoopSpec::opacity(1.4, 0.0, 0.35, 1.0))
    } else {
        dot
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
fn inbox_button(desk: &Desk, ink: &Ink) -> El {
    let waiting = desk.inbox_items().len();
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

fn session_header(desk: &Desk, id: u32, name: &str, current: bool, ink: &Ink) -> El {
    let key = desk
        .topology
        .iter()
        .flat_map(|t| t.sessions.iter())
        .find(|s| s.id == id)
        .and_then(|s| s.key);
    let label = name.to_owned();
    let waiting = desk
        .topology
        .iter()
        .flat_map(|t| t.panes.iter())
        .filter(|p| p.session_id == id && desk.activities.mark(&p.terminal_id).needs_you())
        .count();
    let header = row()
        .key(format!("session-{id}"))
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
            text(name.to_uppercase())
                .font(TextStyle::new(10.5, 650.0).tracking(0.6))
                .color(if current { ink.text } else { ink.dim })
                .truncate(),
        )
        .child(el().grow(1.0));
    if waiting == 0 {
        return header;
    }
    header.child(
        el().px(6.0)
            .radius(8.0)
            .fill(mark_color(Mark::Blocked, ink).alpha(0.22))
            .child(
                text(waiting.to_string())
                    .font(CAPTION)
                    .color(mark_color(Mark::Blocked, ink)),
            ),
    )
}

fn pane_row(
    desk: &Desk,
    pane: &phux_client_runtime::control::PaneDescriptor,
    selected: bool,
    ink: &Ink,
) -> El {
    let mark = desk.activities.mark(&pane.terminal_id);
    let agent = desk
        .activities
        .agent_name(&pane.terminal_id)
        .map(str::to_owned);
    let (id, session) = (pane.terminal_id.clone(), pane.session_name.clone());
    let accent = mark_color(mark, ink);
    let mut line = row()
        .w_full()
        .gap(6.0)
        .items_center()
        .child(dot(mark, ink))
        .child(
            text(pane_label(pane))
                .font(if selected { LABEL_STRONG } else { LABEL })
                .color(ink.text)
                .truncate(),
        )
        .child(el().grow(1.0));
    if let Some(agent) = agent {
        line = line.child(
            el().px(6.0)
                .radius(4.0)
                .fill(accent.alpha(0.16))
                .child(text(agent).font(CAPTION).color(accent)),
        );
    }
    line = line.child(
        text(pane.terminal_id.to_string())
            .font(CAPTION)
            .color(ink.dim),
    );
    let detail = (mark != Mark::None && mark != Mark::Idle).then(|| {
        row()
            .w_full()
            .pl(20.0)
            .child(text(mark.word()).font(CAPTION).color(accent))
    });
    col()
        .key(format!("pane-{}", pane.terminal_id))
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

pub fn view(desk: &Desk, ink: &Ink, inset: f32) -> El {
    let mut list = col().w_full().gap(2.0).px(8.0);
    let shown = desk.shown.as_ref().map(|s| &s.terminal);
    let current = desk.selected_session_name();
    if let Some(topology) = &desk.topology {
        for session in &topology.sessions {
            list = list.child(session_header(
                desk,
                session.id,
                &session.name,
                current.as_deref() == Some(session.name.as_str()),
                ink,
            ));
            for pane in topology
                .panes
                .iter()
                .filter(|pane| pane.session_id == session.id)
            {
                list = list.child(pane_row(desk, pane, shown == Some(&pane.terminal_id), ink));
            }
        }
    }
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
                .child(inbox_button(desk, ink)),
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
