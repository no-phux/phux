//! Agent state as colour, and the attention mark that sits on the thing that
//! needs a person: a tab, a session row, a terminal row. Attention is never a
//! separate list; it is overlaid where the work already is.

use gpui::prelude::*;
use gpui::{Hsla, div, px};

use crate::connection::{AgentState, state_rank};

use super::super::color::from_hex;
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
