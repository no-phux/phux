//! A comment's lines, from Kit's selection to GitHub's thread coordinates.

use gpui_kit::component::diff::{DiffLineRange, DiffSide};

use crate::git::github::{LineSide, NewThread};

use super::Pending;

/// GitHub's side of a Kit side.
pub(super) const fn side(side: DiffSide) -> LineSide {
    match side {
        DiffSide::Original => LineSide::Left,
        DiffSide::Modified => LineSide::Right,
    }
}

/// The row where `side`'s `line` first shows in one file's patch section.
pub(crate) fn patch_row(section: &str, side: LineSide, line: u32) -> Option<usize> {
    let (mut old, mut new, mut in_hunk) = (0_u32, 0_u32, false);
    for (row, text) in section.lines().enumerate() {
        if let Some(header) = text.strip_prefix("@@ ") {
            let mut starts = header.split_whitespace().take(2).map(|range| {
                range
                    .get(1..)?
                    .split(',')
                    .next()
                    .and_then(|n| n.parse::<u32>().ok())
            });
            old = starts.next().flatten()?;
            new = starts.next().flatten()?;
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (on_old, on_new) = match text.as_bytes().first() {
            Some(b'-') => (true, false),
            Some(b'+') => (false, true),
            Some(b'\\') => continue,
            _ => (true, true),
        };
        let here = match side {
            LineSide::Left => on_old && old == line,
            LineSide::Right => on_new && new == line,
        };
        if here {
            return Some(row);
        }
        old += u32::from(on_old);
        new += u32::from(on_new);
    }
    None
}

/// A Kit range as GitHub's thread lines, the start first. On one side that
/// is the lower line; across both it is the one earlier in the patch, read
/// from the file's `section`, and the old side when the patch cannot say.
pub(crate) fn thread_lines(
    range: &DiffLineRange,
    section: Option<&str>,
) -> ((LineSide, u32), (LineSide, u32)) {
    let start = (side(range.side()), range.start() as u32);
    let end = (side(range.end_side()), range.end() as u32);
    let ordered = if start.0 == end.0 {
        start.1 <= end.1
    } else {
        let row = |(side, line)| section.and_then(|section| patch_row(section, side, line));
        match (row(start), row(end)) {
            (Some(a), Some(b)) => a <= b,
            _ => start.0 == LineSide::Left,
        }
    };
    if ordered { (start, end) } else { (end, start) }
}

pub(super) fn new_thread(pending: &Pending, section: Option<&str>) -> NewThread {
    let (start, (side, line)) = thread_lines(&pending.range, section);
    NewThread {
        path: pending.range.path().to_string(),
        side,
        line,
        start: Some(start),
        body: pending.body.clone(),
    }
}
