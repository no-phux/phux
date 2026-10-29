//! The window: a session sidebar, the terminal, and a status line.

use phux_client_runtime::control::Status;
use rune::tok;
use rune::{Color, Cx, El, Family, TextStyle, col, el, row, text};

use crate::desk::Desk;

mod palette;
mod panes;
mod sidebar;

const SIDEBAR_W: f32 = 236.0;
/// Space between the terminal area's edge and its grid.
pub(crate) const PAD: f32 = 6.0;
pub(crate) const LABEL: TextStyle = TextStyle::new(12.0, 400.0);
pub(crate) const LABEL_STRONG: TextStyle = TextStyle::new(12.0, 600.0);
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
                        .child(sidebar::view(desk, &p, inset))
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
                .child(panes::view(desk, &p, cx))
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

pub(crate) fn button(label: &'static str, key: &'static str, p: &Ink, press: fn(&mut Desk)) -> El {
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

pub(crate) fn placeholder(desk: &Desk, p: &Ink) -> El {
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
