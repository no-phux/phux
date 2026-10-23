//! The tiny split-layout map beside a tab's label.

use gpui::prelude::*;

use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::skin::Look;
use crate::app::ui::agent_swatch;
use crate::app::ui::layout_map::layout_map;
use crate::connection::AgentState;
use crate::workspace::layout::{self, DeskTab, Placement};

/// How a pane's cell in its tab's map is painted.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CellTone {
    /// The pane in front: focused, or the zoomed one.
    Front,
    /// Visible beside the front pane.
    Beside,
    /// Hidden behind a zoomed pane.
    Hidden,
}

fn cell_tone(tab: &DeskTab, placement: &Placement) -> CellTone {
    match tab.zoomed_id.as_deref() {
        Some(zoomed) if zoomed == placement.id => CellTone::Front,
        Some(_) => CellTone::Hidden,
        None if tab.focused_id == placement.id => CellTone::Front,
        None => CellTone::Beside,
    }
}

/// A split tab's map: each pane a cell in its own proportions, the front one
/// lit (in the accent on the selected tab), an agent's cell in its state's
/// colour, and the panes a zoom hides faded. None for a single pane.
pub(super) fn render(desk: &Desk, tab: &DeskTab, selected: bool, look: &Look) -> Option<gpui::Div> {
    if layout::placements(&tab.root).len() < 2 {
        return None;
    }
    let accent = from_hex(&look.chrome.theme.accent);
    let ink = look.ink;
    let paint = |placement: &Placement| {
        let agent = placement
            .terminal_id
            .as_ref()
            .and_then(|id| desk.agent(id))
            .map(|info| info.state)
            .filter(|state| *state != AgentState::Unknown)
            .map(|state| from_hex(agent_swatch(state)));
        match (cell_tone(tab, placement), agent) {
            (CellTone::Hidden, _) => ink.opacity(0.16),
            (CellTone::Front, Some(swatch)) => swatch,
            (CellTone::Front, None) if selected => accent,
            (CellTone::Front, None) => ink.opacity(0.6),
            (CellTone::Beside, Some(swatch)) => swatch.opacity(0.45),
            (CellTone::Beside, None) => ink.opacity(if selected { 0.24 } else { 0.18 }),
        }
    };
    let id = tab.id.clone();
    Some(layout_map(&tab.root, 20., 14., &paint).debug_selector(move || format!("tab-map-{id}")))
}
