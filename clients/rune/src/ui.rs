//! The window: a session sidebar, the terminal, and a status line.

use phux_client_runtime::control::{PaneDescriptor, Status};
use rune::tok;
use rune::{Color, Cx, El, Family, TextStyle, col, el, row, text};

use crate::desk::Desk;
use crate::grid;

const SIDEBAR_W: f32 = 220.0;
/// Space between the terminal area's edge and its grid.
const PAD: f32 = 6.0;
const LABEL: TextStyle = TextStyle::new(12.0, 400.0);
const LABEL_STRONG: TextStyle = TextStyle::new(12.0, 600.0);
const CAPTION: TextStyle = TextStyle::new(11.0, 500.0);

struct Palette {
    bg: Color,
    sidebar: Color,
    text: Color,
    dim: Color,
    separator: Color,
    selected: Color,
    hover: Color,
    accent: Color,
    danger: Color,
}

fn palette(dark: bool, accent: Color) -> Palette {
    let t = tok::theme(dark);
    Palette {
        bg: t.bg,
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
    let p = palette(cx.system_dark(), cx.accent());
    row()
        .w_full()
        .h_full()
        .fill(p.bg)
        .child(sidebar(desk, &p, cx.titlebar_inset()))
        .child(el().w(1.0).h_full().fill(p.separator))
        .child(
            col()
                .grow(1.0)
                .h_full()
                .min_w(0.0)
                .min_h(0.0)
                .child(terminal(desk, &p, cx))
                .child(el().w_full().h(1.0).fill(p.separator))
                .child(status_line(desk, &p)),
        )
}

fn sidebar(desk: &Desk, p: &Palette, inset: f32) -> El {
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
                let label = pane_label(pane);
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
        .child(el().w_full().h(inset.max(12.0)).window_drag())
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

/// A terminal's name, else its title, else its folder.
fn pane_label(pane: &PaneDescriptor) -> String {
    let nonempty = |s: &Option<String>| s.clone().filter(|s| !s.trim().is_empty());
    nonempty(&pane.name)
        .or_else(|| nonempty(&pane.title))
        .or_else(|| {
            nonempty(&pane.cwd).map(|cwd| {
                let trimmed = cwd.trim_end_matches('/');
                trimmed
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("/")
                    .to_owned()
            })
        })
        .unwrap_or_else(|| "terminal".to_owned())
}

fn button(label: &'static str, key: &'static str, p: &Palette, press: fn(&mut Desk)) -> El {
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

fn terminal(desk: &Desk, p: &Palette, cx: &mut Cx<Desk>) -> El {
    let area = el()
        .key("terminal")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .padding(PAD)
        .on_layout(|desk: &mut Desk, rect| {
            desk.laid_out([rect[2] - 2.0 * PAD, rect[3] - 2.0 * PAD])
        });
    let frame = desk
        .client
        .as_ref()
        .zip(desk.shown.as_ref())
        .and_then(|(client, shown)| {
            // Remember the fence this build paints, for the drawn receipt.
            let fence = client.with_control(|control| control.projection_fence(&shown.terminal));
            desk.painted_fence
                .set(fence.map(|f| (shown.terminal.clone(), f)));
            client.acquire_view(shown.view)
        });
    desk.scale.set(cx.env().scale);
    let Some(frame) = frame else {
        return area.fill(p.bg).center().child(placeholder(desk, p));
    };
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
        .child(grid::paint(&frame, desk.focused, &metrics))
}

fn placeholder(desk: &Desk, p: &Palette) -> El {
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

fn status_line(desk: &Desk, p: &Palette) -> El {
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
