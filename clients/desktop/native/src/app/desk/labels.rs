//! Free helpers: labels and titles for panes and tabs, paste targets,
//! drop positions and terminal text regions.

use super::*;

/// Where a dragged tab lands along either axis, excluding its own center.
pub(crate) fn tab_drop_index(from: usize, centers: &[f32], coordinate: f32) -> usize {
    if from >= centers.len() || centers.iter().any(|center| !center.is_finite()) {
        return from;
    }
    let mut to = 0;
    for (index, center) in centers.iter().enumerate() {
        if index != from && coordinate > *center {
            to = if index < from { index + 1 } else { index };
        }
    }
    to
}

pub(crate) fn region_text(
    client: &Client,
    view: phux_client_runtime::ViewId,
    region: TextRegion,
) -> String {
    let Some(engine) = client.engine() else {
        return String::new();
    };
    match engine.view_region_text_bounded(view, region, SEARCH_BYTES) {
        Ok(phux_client_runtime::engine::BoundedSelectionText::Text(bytes)) => {
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => String::new(),
    }
}

/// Whether a confirmed paste may still go to its placement: the placement
/// still shows the terminal the paste was held for.
pub(crate) fn held_paste_target_holds(current: Option<&ResourceId>, held_for: &ResourceId) -> bool {
    current == Some(held_for)
}

/// How long a finished command ran, coarsely: `42s`, `3m 5s`, `1h 2m`.
pub(crate) fn command_span(ran: Duration) -> String {
    let seconds = ran.as_secs();
    match (seconds / 3600, seconds % 3600 / 60, seconds % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

pub(crate) fn short_path(path: Option<String>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    ["/Users/", "/home/"]
        .into_iter()
        .find_map(|home| path.strip_prefix(home)?.split_once('/'))
        .map_or_else(|| path.clone(), |(_, rest)| format!("~/{rest}"))
}

/// How the status bar colours a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Good,
    Busy,
    Bad,
}

/// One title rule for tabs, the sidebar, headers and the palette: the
/// resource's own name, else its title, else its directory's name, else the
/// name of the arrangement group that shows it, else "shell".
pub(crate) fn pane_label(
    pane: Option<&phux_client_runtime::control::PaneDescriptor>,
    group: Option<&str>,
) -> String {
    let Some(pane) = pane else {
        return "shell".into();
    };
    let named = |text: Option<&str>| text.map(display_title).filter(|text| !text.is_empty());
    named(pane.name.as_deref())
        .or_else(|| named(pane.title.as_deref()))
        .or_else(|| {
            named(
                pane.cwd
                    .as_deref()
                    .and_then(|cwd| cwd.rsplit('/').find(|part| !part.is_empty())),
            )
        })
        .or_else(|| named(group))
        .unwrap_or_else(|| "shell".into())
}

/// Why the server ended the attach, in words: a revoked or expired
/// authorization says so even when the server sent no message.
pub(crate) fn detach_detail(
    reason: Option<phux_protocol::wire::frame::DetachReason>,
    message: &str,
) -> String {
    match (reason, message.trim()) {
        (Some(reason), "") => reason.describe().to_owned(),
        (Some(reason), extra) => format!("{}: {extra}", reason.describe()),
        (None, "") => "The server ended the attach without saying why.".to_owned(),
        (None, extra) => extra.to_owned(),
    }
}

/// Longest title, in characters, any chrome surface or the window shows.
pub(crate) const TITLE_CAP: usize = 256;

/// Program-set text made safe to show: no control characters (an OSC title
/// cannot smuggle a newline, escape or `DEL` into the window title or a tab)
/// and no bidi overrides or isolates that would reorder what follows, so a
/// title cannot pose as another file or host. Trimmed and bounded.
pub(crate) fn display_title(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|&ch| !ch.is_control() && !is_bidi_control(ch))
        .take(TITLE_CAP)
        .collect();
    clean.trim().to_owned()
}

/// Explicit direction marks, embeddings, overrides and isolates (the same
/// set `phux ls` and the TUI drop).
pub(crate) const fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

pub fn tab_title(desk: &Desk, tab: &crate::workspace::layout::DeskTab) -> String {
    if let Some(title) = tab
        .title
        .as_deref()
        .map(display_title)
        .filter(|title| !title.is_empty())
    {
        return title;
    }
    let focus = crate::workspace::layout::placements(&tab.root)
        .into_iter()
        .find(|placement| placement.id == tab.focused_id);
    desk.pane_title(focus.and_then(|placement| placement.terminal_id.as_ref()))
}

pub(crate) fn pane_unclipped(
    viewport: gpui::Size<gpui::Pixels>,
    bounds: gpui::Bounds<gpui::Pixels>,
) -> bool {
    let right = bounds.origin.x.as_f32() + bounds.size.width.as_f32();
    let bottom = bounds.origin.y.as_f32() + bounds.size.height.as_f32();
    bounds.origin.x.as_f32() >= 0.
        && bounds.origin.y.as_f32() >= 0.
        && right <= viewport.width.as_f32()
        && bottom <= viewport.height.as_f32()
        && bounds.size.width.as_f32() > 0.
        && bounds.size.height.as_f32() > 0.
}
