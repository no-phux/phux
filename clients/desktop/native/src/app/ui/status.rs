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
use super::atoms::{dot, shadow};

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

/// The wash a row or tab wears while something in it needs a person.
pub(crate) fn attention_wash(state: AgentState) -> Hsla {
    from_hex(agent_swatch(state)).opacity(0.1)
}

/// A row's state at a glance: a lit dot while something needs a person, a
/// quiet one while an agent is otherwise active, nothing at all otherwise.
pub(crate) fn state_dot(attention: Option<AgentState>, agent: Option<AgentState>) -> gpui::Div {
    match (attention, agent) {
        (Some(state), _) => {
            let color = from_hex(agent_swatch(state));
            dot(8., color).shadow(vec![shadow(color.opacity(0.5), 0., 6., 0., false)])
        }
        (None, Some(state)) => dot(6., from_hex(agent_swatch(state)).opacity(0.7)),
        (None, None) => div(),
    }
}

/// A dot pinned to the top-right corner of its (relative) parent, ringed in
/// the sheet so it reads over any wash. The parent stays the click target.
pub(crate) fn attention_mark(state: AgentState, look: &Look) -> gpui::Div {
    let color = from_hex(agent_swatch(state));
    div()
        .absolute()
        .top(px(-2.))
        .right(px(-2.))
        .size(px(8.))
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(look.sheet.opacity(1.))
        .shadow(vec![shadow(color.opacity(0.45), 0., 6., 0., false)])
}

/// A harness mark's colour in this theme: its own tint, else muted ink.
pub(crate) fn harness_tint(harness: Harness, look: &Look) -> Hsla {
    harness.tint().map_or_else(
        || from_hex(&look.chrome.muted),
        |[light, dark]| from_hex(if look.light { light } else { dark }),
    )
}

/// Which harness drives a pane: its mark `size` points across, with the
/// agent's calm `state` as a small dot on its lower corner.
pub(crate) fn harness_mark(
    harness: Harness,
    state: Option<AgentState>,
    size: f32,
    look: &Look,
) -> gpui::Div {
    let pip = (size * 0.45).max(4.);
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
        .children(
            state
                .filter(|state| *state != AgentState::Unknown)
                .map(|state| {
                    dot(pip, from_hex(agent_swatch(state)))
                        .absolute()
                        .right(px(-pip / 3.))
                        .bottom(px(-pip / 3.))
                        .border_1()
                        .border_color(look.sheet.opacity(1.))
                }),
        )
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
