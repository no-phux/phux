//! Shell prompt classification from the terminal's own semantic marks.

/// Where the replica's shell is in its OSC 133 cycle: at a prompt when the
/// cursor sits on prompt or input content, running when prompt marks exist
/// elsewhere on the primary screen, and unknown without marks. The alternate
/// screen carries no marks, so a full-screen program reads as unknown.
pub(in crate::engine::owner) fn replica_prompt(
    terminal: &libghostty_vt::Terminal<'static, 'static>,
) -> crate::engine::ReplicaPrompt {
    use crate::engine::ReplicaPrompt;
    let primary = matches!(
        terminal.active_screen(),
        Ok(libghostty_vt::screen::Screen::Primary)
    );
    if !primary {
        return ReplicaPrompt::Unknown;
    }
    if terminal.is_cursor_at_prompt().unwrap_or(false) || cursor_row_marked(terminal) {
        return ReplicaPrompt::AtPrompt;
    }
    if any_prompt_row(terminal) {
        ReplicaPrompt::Running
    } else {
        ReplicaPrompt::Unknown
    }
}

/// Whether the cursor's row holds prompt or input cells, as a prompt that a
/// bootstrap redrew without the cursor's own semantic state still does.
fn cursor_row_marked(terminal: &libghostty_vt::Terminal<'static, 'static>) -> bool {
    use libghostty_vt::point::{Coordinate, Point};
    use libghostty_vt::screen::{CellSemanticContent, RowSemanticPrompt};
    let (Ok(y), Ok(cols)) = (terminal.cursor_y(), terminal.cols()) else {
        return false;
    };
    let at = |x: u16| Point::Active(Coordinate { x, y: u32::from(y) });
    let row_marked = terminal
        .grid_ref(at(0))
        .and_then(|cell| cell.row())
        .and_then(libghostty_vt::screen::Row::semantic_prompt)
        .is_ok_and(|prompt| prompt != RowSemanticPrompt::None);
    row_marked
        || (0..cols).any(|x| {
            terminal
                .grid_ref(at(x))
                .and_then(|cell| cell.cell())
                .and_then(libghostty_vt::screen::Cell::semantic_content)
                .is_ok_and(|content| {
                    matches!(
                        content,
                        CellSemanticContent::Prompt | CellSemanticContent::Input
                    )
                })
        })
}

/// Whether any row of the primary screen, scrollback included, carries a
/// prompt mark. Scans upward from the bottom, where the latest prompt is.
fn any_prompt_row(terminal: &libghostty_vt::Terminal<'static, 'static>) -> bool {
    use libghostty_vt::point::{Coordinate, Point};
    use libghostty_vt::screen::RowSemanticPrompt;
    let Ok(total) = terminal.total_rows() else {
        return false;
    };
    (0..total).rev().any(|row| {
        u32::try_from(row).is_ok_and(|y| {
            terminal
                .grid_ref(Point::Screen(Coordinate { x: 0, y }))
                .and_then(|cell| cell.row())
                .and_then(libghostty_vt::screen::Row::semantic_prompt)
                .is_ok_and(|prompt| prompt != RowSemanticPrompt::None)
        })
    })
}
