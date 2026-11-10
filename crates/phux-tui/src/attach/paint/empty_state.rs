//! Screens with no pane to draw: the empty and not-arranged session states,
//! and the centered message lines they (and placeholder tiles) are made of.

use std::io::Write;

use super::{
    ChromeCtx, FrameBlock, StatusBarPaint, end_of_frame_cursor, paint_bar_after_pane,
    repaint_sidebar_strip, seal_frame,
};

/// The empty state's title line.
const EMPTY_SESSION_TITLE: &str = "Empty session";
/// The empty state's explanation line.
const EMPTY_SESSION_BODY: &str = "Open a new tab to start a terminal.";
/// The not-arranged state's explanation line.
const UNARRANGED_BODY: &str = "Hidden panes keep their terminals running.";

/// The lines a persistent session with no members shows. The hint names the
/// chord bound to `new-window`, or the command palette when no chord is
/// bound.
pub(in crate::attach) fn empty_session_lines(new_window_chord: Option<&str>) -> [String; 3] {
    let hint = new_window_chord.map_or_else(
        || "Run New tab from the command palette.".to_owned(),
        |chord| format!("{chord}  new tab"),
    );
    [
        EMPTY_SESSION_TITLE.to_owned(),
        EMPTY_SESSION_BODY.to_owned(),
        hint,
    ]
}

/// The lines a session shows when `running` members have no view (R15). The
/// hint names the chord bound to `reopen-views`, or the command palette when
/// no chord is bound.
pub(in crate::attach) fn unarranged_lines(
    running: usize,
    reopen_chord: Option<&str>,
) -> [String; 3] {
    let hint = reopen_chord.map_or_else(
        || "Run reopen-views from the command palette.".to_owned(),
        |chord| format!("{chord}  reopen views"),
    );
    [
        format!("{running} running, not arranged"),
        UNARRANGED_BODY.to_owned(),
        hint,
    ]
}

/// Paint a session with nothing arranged: the chrome as usual and the
/// empty-state lines ([`empty_session_lines`] or [`unarranged_lines`])
/// centered in the content area. One
/// frame block like [`super::paint_full_frame`]; the cursor ends parked and hidden
/// at the content origin, since there is no pane to own it.
pub(in crate::attach) fn paint_empty_session<W: crate::attach::RenderSink>(
    out: &mut W,
    chrome: &mut ChromeCtx<'_>,
    lines: &[String],
) -> StatusBarPaint {
    let mut block = FrameBlock::begin(out);
    let content = chrome.content_layout().rect;
    let origin = Some((content.x, content.y));
    let _ = block.write_all(b"\x1b[2J\x1b[H");
    write_centered_lines(&mut block, content, lines);
    // No grid is drawn here, so the strip's rule has nothing to tee into.
    repaint_sidebar_strip(&mut block, chrome, &[]);
    let painted = paint_bar_after_pane(&mut block, chrome, None, origin, true);
    let composed = end_of_frame_cursor(&mut block, None, origin).is_ok();
    seal_frame(block, painted, composed, chrome.status_bar.as_deref_mut())
}

/// Write `lines` centered in `rect`, each clipped to the rect's width.
pub(in crate::attach) fn write_centered_lines<W: Write>(
    out: &mut W,
    rect: crate::layout::Rect,
    lines: &[String],
) {
    let count = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let top = rect.y.saturating_add(rect.h.saturating_sub(count) / 2);
    let bottom = rect.y.saturating_add(rect.h);
    for (row, line) in (top..bottom).zip(lines) {
        let text: String = line.chars().take(usize::from(rect.w)).collect();
        let width = u16::try_from(text.chars().count()).unwrap_or(rect.w);
        let col = rect.x.saturating_add(rect.w.saturating_sub(width) / 2);
        let _ = crate::attach::render::write_cup(out, row, col);
        let _ = out.write_all(text.as_bytes());
    }
}
