//! Agent state as colour, and the attention mark that sits on the thing that
//! needs a person: a tab, a session row, a terminal row. Attention is never a
//! separate list; it is overlaid where the work already is.

use gpui::prelude::*;
use gpui::{Hsla, div, px};
use gpui_kit::component::Icon;

use crate::connection::{AgentState, state_rank};

use super::super::color::from_hex;
use super::super::pane_identity::Harness;
use super::super::skin::Look;
use super::atoms::dot;

/// Agent lifecycle colours stay fixed across themes.
pub(crate) fn agent_swatch(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "#f9e2af",
        AgentState::Blocked => "#f38ba8",
        AgentState::Done => "#94e2d5",
        AgentState::Idle => "#a6e3a1",
        AgentState::Unknown => "#7f849c",
    }
}

/// The most pressing of `states`.
pub(crate) fn strongest(states: impl IntoIterator<Item = AgentState>) -> Option<AgentState> {
    states.into_iter().min_by_key(|state| state_rank(*state))
}

/// Only actionable attention earns a dot. Routine lifecycle state remains in
/// the tooltip instead of adding another signal beside every name.
pub(crate) fn state_dot(attention: Option<AgentState>, _agent: Option<AgentState>) -> gpui::Div {
    match attention {
        Some(state) => dot(4., from_hex(agent_swatch(state))),
        None => div(),
    }
}

/// A harness mark's colour in this theme: its own tint, else muted ink.
pub(crate) fn harness_tint(harness: Harness, look: &Look) -> Hsla {
    harness.tint().map_or_else(
        || from_hex(&look.chrome.muted),
        |[light, dark]| from_hex(if look.light { light } else { dark }),
    )
}

/// Which harness drives a pane. Ordinary lifecycle state adds no decoration.
pub(crate) fn harness_mark(
    harness: Harness,
    _state: Option<AgentState>,
    size: f32,
    look: &Look,
) -> gpui::Div {
    harness_status(harness, None, None, size, look)
}

/// A retained identity with independent attention. Urgency keeps its pip even
/// when lifecycle state is unknown; routine states have no pip.
pub(crate) fn harness_status(
    harness: Harness,
    _state: Option<AgentState>,
    attention: Option<AgentState>,
    size: f32,
    look: &Look,
) -> gpui::Div {
    let pip = 4.;
    div()
        .debug_selector(move || format!("harness-mark-{harness:?}"))
        .flex_shrink_0()
        .relative()
        .size(px(size))
        .child(
            Icon::new(harness.glyph())
                .size(px(size))
                .text_color(harness_tint(harness, look)),
        )
        .children(attention.map(|state| {
            let color = from_hex(agent_swatch(state));
            dot(pip, color)
                .absolute()
                .right(px(-pip / 3.))
                .bottom(px(-pip / 3.))
                .border_1()
                .border_color(look.sheet.opacity(1.))
                .debug_selector(|| "harness-attention".into())
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_outranks_done_and_working() {
        assert_eq!(
            strongest([AgentState::Working, AgentState::Blocked, AgentState::Done]),
            Some(AgentState::Blocked)
        );
        assert_eq!(strongest([]), None);
    }
}
