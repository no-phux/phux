//! The window's words about itself: the status line, the bar an action
//! waits in, and what stands in for a terminal that isn't there.

use phux_client_runtime::control::Status;
use rune::{Color, El, Family, TextStyle, col, el, row, text};

use super::{CAPTION, Ink, LABEL, button, keep_from_pane};
use crate::desk::{Desk, Notice, NoticeKind, Pending};

/// A notice's words, wrapping; with none it draws nothing.
pub fn notice_line(notice: Option<&Notice>, p: &Ink) -> El {
    let (line, kind) = notice.map_or((String::new(), NoticeKind::Error), |notice| {
        (notice.text.clone(), notice.kind)
    });
    let color = match kind {
        NoticeKind::Info => p.text,
        NoticeKind::Error => p.danger,
    };
    text(line)
        .font(CAPTION)
        .color(color)
        .wrap()
        .grow(1.0)
        .min_w(0.0)
}

/// What a held action would do, and the word for doing it.
fn question(pending: &Pending) -> (String, &'static str) {
    match pending {
        Pending::Paste(held) => {
            let lines = held.lines().count().max(1);
            let plural = if lines == 1 { "" } else { "s" };
            (
                format!(
                    "This paste has {lines} line{plural} and would run as typed commands. Paste anyway?"
                ),
                "Paste (Return)",
            )
        }
        Pending::Close(terminal) => (
            format!("Close {terminal}? Anything running in it ends."),
            "Close (Return)",
        ),
        Pending::CloseTab(terminals) if terminals.len() == 1 => (
            "Close this tab? Anything running in it ends.".to_owned(),
            "Close (Return)",
        ),
        Pending::CloseTab(terminals) => (
            format!(
                "Close this tab and its {} terminals? Anything running in them ends.",
                terminals.len()
            ),
            "Close (Return)",
        ),
        Pending::CloseSession(_, name) => (
            format!("End session {name} and everything running in it?"),
            "End session (Return)",
        ),
    }
}

/// The action waiting on Return or Escape, floating over the terminals so
/// asking never resizes them.
pub fn confirm_bar(desk: &Desk, p: &Ink) -> Option<El> {
    let (message, verb) = question(desk.pending.as_ref()?);
    Some(
        row()
            .key("confirm-bar")
            .on_drag(keep_from_pane)
            .pin(Some(0.0), Some(0.0), Some(0.0), None)
            .px(12.0)
            .py(8.0)
            .gap(8.0)
            .items_center()
            .fill(p.raised)
            .border(1.0, p.edge)
            .shadow(6.0, 18.0, Color::BLACK.alpha(0.35))
            .transition(rune::Transition::opacity())
            .child(
                text(message)
                    .font(LABEL)
                    .color(p.text)
                    .wrap()
                    .grow(1.0)
                    .min_w(0.0),
            )
            .child(
                button("Cancel (Esc)", "confirm-cancel", p, Desk::cancel_pending)
                    .grow(0.0)
                    .px(10.0),
            )
            .child(
                button(verb, "confirm-ok", p, Desk::confirm_pending)
                    .grow(0.0)
                    .px(10.0),
            ),
    )
}

pub fn placeholder(desk: &Desk, p: &Ink) -> El {
    let (title, detail, color) = match (&desk.error, desk.status) {
        (Some(error), _) => ("Can't reach phux", error.clone(), p.danger),
        (None, Status::Idle | Status::Connecting) => {
            // A machine that's away keeps retrying; say what it answered.
            let why = desk.client.as_ref().and_then(|client| client.last_error());
            ("Connecting…", why.unwrap_or_default(), p.dim)
        }
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
        .child(el().h(6.0))
        .child(
            button(
                "Other machines…",
                "placeholder-machines",
                p,
                Desk::toggle_machines,
            )
            .grow(0.0)
            .px(12.0),
        )
}

pub fn status_line(desk: &Desk, p: &Ink) -> El {
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
        // Where a ⌘-click goes, as a browser shows it.
        .children(desk.pointer.hover.as_ref().map(|(_, link)| {
            text(link.url.clone())
                .font(CAPTION)
                .color(p.accent)
                .truncate()
        }))
        .children(super::updates::pill(desk, p))
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

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::ResourceId;

    #[test]
    fn a_held_tab_close_counts_its_terminals() {
        let close = |n| Pending::CloseTab((0..n).map(ResourceId::local).collect());
        assert_eq!(
            question(&close(1)).0,
            "Close this tab? Anything running in it ends."
        );
        assert_eq!(
            question(&close(3)).0,
            "Close this tab and its 3 terminals? Anything running in them ends."
        );
    }
}
