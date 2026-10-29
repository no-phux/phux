//! The window: a session sidebar, the terminal, and a status line.

use phux_client_runtime::control::Status;
use rune::tok;
use rune::{Color, Cx, El, Family, TextStyle, col, el, row, text};

use crate::desk::Desk;

mod palette;
use crate::grid;

const SIDEBAR_W: f32 = 220.0;
/// Space between the terminal area's edge and its grid.
const PAD: f32 = 6.0;
pub(crate) const LABEL: TextStyle = TextStyle::new(12.0, 400.0);
const LABEL_STRONG: TextStyle = TextStyle::new(12.0, 600.0);
pub(crate) const CAPTION: TextStyle = TextStyle::new(11.0, 500.0);

pub(crate) struct Ink {
    pub(crate) bg: Color,
    pub(crate) raised: Color,
    pub(crate) sidebar: Color,
    pub(crate) text: Color,
    pub(crate) dim: Color,
    pub(crate) separator: Color,
    pub(crate) selected: Color,
    pub(crate) hover: Color,
    pub(crate) accent: Color,
    pub(crate) danger: Color,
}

fn ink(dark: bool, accent: Color) -> Ink {
    let t = tok::theme(dark);
    Ink {
        bg: t.bg,
        raised: t.card,
        sidebar: t.card,
        text: t.text,
        dim: t.text_secondary,
        separator: t.separator,
        selected: accent.alpha(0.22),
        hover: t.hover,
        accent,
        danger: rune::oklch(0.68, 0.17, 25.0),
    }
}

pub fn view(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let p = ink(cx.system_dark(), cx.accent());
    let inset = cx.titlebar_inset().max(12.0);
    // With the sidebar folded, the traffic lights sit over the terminal's
    // top edge: give them a drag strip.
    let top = (!desk.sidebar_open).then(|| el().key("titlebar").w_full().h(inset).window_drag());
    let main = row()
        .key("main")
        .w_full()
        .h_full()
        .child(
            el().key("sidebar-host")
                .w(if desk.sidebar_open {
                    SIDEBAR_W + 1.0
                } else {
                    0.0
                })
                .h_full()
                .clip_rounded(0.0)
                .motion(tok::motion::SNAPPY)
                .child(
                    row()
                        .w(SIDEBAR_W + 1.0)
                        .h_full()
                        .child(sidebar(desk, &p, inset))
                        .child(el().w(1.0).h_full().fill(p.separator)),
                ),
        )
        .child(
            col()
                .grow(1.0)
                .h_full()
                .min_w(0.0)
                .min_h(0.0)
                .children(top)
                .children(paste_bar(desk, &p))
                .child(terminal(desk, &p, cx))
                .child(el().w_full().h(1.0).fill(p.separator))
                .child(status_line(desk, &p)),
        );
    el().w_full()
        .h_full()
        .fill(p.bg)
        // Rune hands the root pastes nothing focused took (Edit ▸ Paste).
        .on_paste(|desk: &mut Desk, text, _| desk.paste(text))
        .child(main)
        .children(desk.palette_open.then(|| palette::view(desk, &p)))
}

fn sidebar(desk: &Desk, p: &Ink, inset: f32) -> El {
    let mut list = col().w_full().gap(2.0).px(8.0);
    let shown = desk.shown.as_ref().map(|s| &s.terminal);
    let attached = desk.selected_session_name();
    if let Some(topology) = &desk.topology {
        for session in &topology.sessions {
            let current = attached.as_deref() == Some(session.name.as_str());
            list = list.child(
                row()
                    .key(format!("session-{}", session.id))
                    .w_full()
                    .pt(10.0)
                    .pb(4.0)
                    .px(6.0)
                    .gap(6.0)
                    .items_center()
                    .child(el().size(6.0, 6.0).radius(3.0).fill(if current {
                        p.accent
                    } else {
                        p.separator
                    }))
                    .child(
                        text(session.name.clone())
                            .font(CAPTION)
                            .color(p.dim)
                            .truncate(),
                    ),
            );
            for pane in topology
                .panes
                .iter()
                .filter(|pane| pane.session_id == session.id)
            {
                let selected = shown == Some(&pane.terminal_id);
                let label = crate::palette::pane_label(pane);
                let handle = format!("{}", pane.terminal_id);
                let (id, session_name) = (pane.terminal_id.clone(), pane.session_name.clone());
                list = list.child(
                    el().key(format!("pane-{}", pane.terminal_id))
                        .w_full()
                        .px(10.0)
                        .py(6.0)
                        .radius(6.0)
                        .fill(if selected {
                            p.selected
                        } else {
                            Color::TRANSPARENT
                        })
                        .hover(p.hover)
                        .cursor(rune::Cursor::Pointer)
                        .on_press(move |desk: &mut Desk| desk.show(id.clone(), &session_name))
                        .child(
                            row()
                                .w_full()
                                .gap(6.0)
                                .items_center()
                                .child(
                                    text(label)
                                        .font(if selected { LABEL_STRONG } else { LABEL })
                                        .color(p.text)
                                        .truncate(),
                                )
                                .child(el().grow(1.0))
                                .child(text(handle).font(CAPTION).color(p.dim)),
                        ),
                );
            }
        }
    }
    col()
        .w(SIDEBAR_W)
        .h_full()
        .fill(p.sidebar)
        .child(el().w_full().h(inset).window_drag())
        .child(
            row()
                .w_full()
                .px(14.0)
                .pb(6.0)
                .items_center()
                .justify_between()
                .child(text("phux").font(TextStyle::new(13.0, 700.0)).color(p.text))
                .child(text("rune alpha").font(CAPTION).color(p.accent)),
        )
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
                .child(button("New terminal", "new-terminal", p, |d: &mut Desk| {
                    d.new_terminal()
                }))
                .child(button("New session", "new-session", p, |d: &mut Desk| {
                    d.new_session()
                })),
        )
}

fn button(label: &'static str, key: &'static str, p: &Ink, press: fn(&mut Desk)) -> El {
    el().key(key)
        .grow(1.0)
        .py(6.0)
        .radius(6.0)
        .center()
        .border(1.0, p.separator)
        .hover(p.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(press)
        .child(text(label).font(CAPTION).color(p.text))
}

fn terminal(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    let area = el()
        .key("terminal")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .padding(PAD)
        .on_layout(|desk: &mut Desk, rect| {
            desk.laid_out([rect[2] - 2.0 * PAD, rect[3] - 2.0 * PAD])
        });
    desk.scale.set(cx.env().scale);
    let shown = desk.client.as_ref().zip(desk.shown.as_ref());
    let Some((frame, view)) =
        shown.and_then(|(client, shown)| client.acquire_view(shown.view).map(|f| (f, shown.view)))
    else {
        desk.painted.replace(None);
        return area.fill(p.bg).center().child(placeholder(desk, p));
    };
    // The exact frame this build paints, for the drawn receipt.
    desk.painting(view, &frame);
    let metrics = desk.metrics();
    area.fill(grid::background(&frame))
        .cursor(rune::Cursor::Text)
        .on_wheel(|desk: &mut Desk, event, _| {
            desk.wheel(event.dy);
            true
        })
        .on_drag(|desk: &mut Desk, drag, _| {
            // Drag points include the padding; the grid starts inside it.
            desk.select(drag.phase, drag.x - PAD, drag.y - PAD, drag.clicks);
        })
        .child(
            el().child(grid::paint(&frame, desk.focused, desk.blink_on, &metrics))
                .children(
                    (desk.focused && !desk.palette_open && desk.held_paste.is_none()).then(|| {
                        grid::ime_layer(
                            &frame,
                            desk.preedit.as_deref(),
                            &metrics,
                            (p.text, p.raised),
                        )
                    }),
                ),
        )
}

/// A held paste: what it would do, and the choice.
fn paste_bar(desk: &Desk, p: &Ink) -> Option<El> {
    let held = desk.held_paste.as_ref()?;
    let lines = held.lines().count().max(1);
    let message = format!(
        "This paste has {lines} line{} and would run as typed commands. Paste anyway?",
        if lines == 1 { "" } else { "s" }
    );
    Some(
        row()
            .key("paste-bar")
            .w_full()
            .px(12.0)
            .py(8.0)
            .gap(8.0)
            .items_center()
            .fill(p.selected)
            .child(text(message).font(LABEL).color(p.text).truncate())
            .child(el().grow(1.0))
            .child(
                button("Cancel (Esc)", "paste-cancel", p, Desk::cancel_paste)
                    .grow(0.0)
                    .px(10.0),
            )
            .child(
                button("Paste (Return)", "paste-confirm", p, Desk::confirm_paste)
                    .grow(0.0)
                    .px(10.0),
            ),
    )
}

fn placeholder(desk: &Desk, p: &Ink) -> El {
    let (title, detail, color) = match (&desk.error, desk.status) {
        (Some(error), _) => ("Can't reach phux", error.clone(), p.danger),
        (None, Status::Idle | Status::Connecting) => ("Connecting…", String::new(), p.dim),
        (None, _) if desk.topology.as_ref().is_some_and(|t| t.panes.is_empty()) => (
            "No terminals",
            "Create a session or terminal from the sidebar.".to_owned(),
            p.dim,
        ),
        (None, _) => ("Opening terminal…", String::new(), p.dim),
    };
    col()
        .gap(6.0)
        .items_center()
        .max_w(520.0)
        .child(text(title).font(TextStyle::new(15.0, 600.0)).color(color))
        .child(text(detail).font(LABEL).color(p.dim).wrap())
}

fn status_line(desk: &Desk, p: &Ink) -> El {
    let (word, color) = match desk.status {
        Status::Attached => ("attached", p.accent),
        Status::Idle | Status::Connecting => ("connecting", p.dim),
        Status::Negotiated => ("negotiated", p.dim),
        Status::Closed => ("closed", p.dim),
        Status::Failed => ("failed", p.danger),
    };
    let geometry = desk
        .client
        .as_ref()
        .zip(desk.shown.as_ref())
        .and_then(|(client, shown)| client.acquire_view(shown.view))
        .map(|frame| format!("{}×{}", frame.cols, frame.rows))
        .unwrap_or_default();
    let session = desk.selected_session_name().unwrap_or_default();
    row()
        .w_full()
        .h(24.0)
        .px(10.0)
        .gap(14.0)
        .items_center()
        .child(text(word).font(CAPTION).color(color))
        .child(text(session).font(CAPTION).color(p.dim).truncate())
        .child(el().grow(1.0))
        .child(
            text(desk.notice.clone().unwrap_or_default())
                .font(CAPTION)
                .color(p.danger)
                .truncate(),
        )
        .child(
            text(geometry)
                .font(CAPTION.family(Family::SystemMono))
                .color(p.dim),
        )
        .child(
            text(format!("{:.0} pt", desk.font_size))
                .font(CAPTION)
                .color(p.dim),
        )
}
