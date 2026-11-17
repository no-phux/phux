//! Chrome layer: status bar, dividers, sidebar. The badge vocabulary is
//! shared ([`agent_badge`]) so a pane reads the same on its title and its
//! sidebar row.

pub mod dividers;
pub mod sidebar;
pub mod sidebar_sections;
pub mod status_bar;

#[cfg(test)]
mod gallery_tests;

use phux_design::{Agent, State, ToneInput};
use ratatui::style::Color;

use crate::render::theme::{Glyphs, Theme};
use phux_client::agent_meta::AgentMetaState;

/// How one agent's state is drawn anywhere: shape carries the state and
/// colour reinforces it, so a colour-blind reader (or `NO_COLOR`) can
/// still tell them apart (spec §1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentBadge {
    /// The single-cell glyph (the ladder's Unicode or ASCII column).
    pub glyph: &'static str,
    /// Its theme colour.
    pub color: Color,
    /// `true` when the badge should also be bold: the row needs a human
    /// right now, failed, or holds an unread result.
    pub emphatic: bool,
}

/// Filled dot: needs you (blocked, or a pane that asked for a human).
pub(crate) const AGENT_BLOCKED_GLYPH: &str = "●";
/// Half-filled ring: actively working, not waiting.
pub(crate) const AGENT_WORKING_GLYPH: &str = "◐";
/// Filled diamond: finished, and nobody has looked yet (the driver tests
/// pin the tab badge to it).
#[cfg(test)]
pub(crate) const AGENT_DONE_GLYPH: &str = "◆";
/// Hollow ring: idle, or done and seen.
pub(crate) const AGENT_IDLE_GLYPH: &str = "○";

/// The one glyph for a declared agent state, shared by the sidebar, the tab
/// bar, pane titles and the fleet picker (the Unicode ladder column).
#[must_use]
pub(crate) fn agent_state_glyph(state: AgentMetaState) -> &'static str {
    match state {
        AgentMetaState::Unknown => AGENT_IDLE_GLYPH,
        known => state_of(&input(known, false, false)).mark().glyph,
    }
}

/// The vocabulary word for a declared agent state (`needs you`, never the
/// protocol's `blocked`); `None` for unknown, which shows the provider's
/// name instead.
#[must_use]
pub(crate) fn state_word(state: AgentMetaState) -> Option<&'static str> {
    state_of(&input(state, false, false)).word()
}

/// The protocol state as the design language's agent fact.
pub(crate) const fn agent_fact(state: AgentMetaState) -> Agent {
    match state {
        AgentMetaState::Working => Agent::Working,
        AgentMetaState::Blocked => Agent::Blocked,
        AgentMetaState::Idle => Agent::Idle,
        AgentMetaState::Done => Agent::Done,
        AgentMetaState::Unknown => Agent::Unknown,
    }
}

fn input(state: AgentMetaState, attention: bool, seen: bool) -> ToneInput {
    ToneInput {
        agent: Some(agent_fact(state)),
        attention,
        seen,
        ..ToneInput::default()
    }
}

fn state_of(input: &ToneInput) -> State {
    phux_design::tone_of(input)
}

/// The ladder mark for `state` in the theme's glyph column.
#[must_use]
pub fn mark(theme: &Theme, state: State) -> &'static str {
    let row = state.mark();
    match theme.term.glyphs {
        Glyphs::Unicode => row.glyph,
        Glyphs::Ascii => row.ascii,
    }
}

/// The slot a ladder state paints in.
#[must_use]
pub const fn state_color(theme: &Theme, state: State) -> Color {
    match state {
        State::NeedsYou => theme.agent_blocked,
        State::Failed | State::Lost => theme.agent_failed,
        State::Done | State::DoneSeen => theme.agent_done,
        State::Working => theme.agent_working,
        State::Idle => theme.agent_idle,
        // `unknown` and `ended` draw in `text.faint`.
        State::Unknown | State::Ended => theme.sidebar_section,
    }
}

/// The badge for one ladder state.
#[must_use]
pub fn state_badge(theme: &Theme, state: State) -> AgentBadge {
    AgentBadge {
        glyph: mark(theme, state),
        color: state_color(theme, state),
        emphatic: state.mark().bold,
    }
}

/// Resolve the badge for one pane from its facts through
/// [`phux_design::tone_of`]: `●` needs you, `✕` failed, `◆` done and
/// unread, `◐` working, `○` idle or done and seen, `·` unknown, `–` ended.
#[must_use]
pub fn pane_badge(theme: &Theme, facts: &ToneInput) -> AgentBadge {
    state_badge(theme, state_of(facts))
}

/// [`pane_badge`] for an agent pane's declared state, attention and seen
/// flag.
#[must_use]
pub fn agent_badge(
    theme: &Theme,
    state: AgentMetaState,
    attention: bool,
    seen: bool,
) -> AgentBadge {
    pane_badge(theme, &input(state, attention, seen))
}

/// The badge for a non-agent pane that asked for a human (ADR-0035): the
/// same filled dot as an agent that needs you.
#[must_use]
pub fn attention_badge(theme: &Theme) -> AgentBadge {
    state_badge(theme, State::NeedsYou)
}

/// The mark and its count for the status bar, where there is no room for
/// the word: ` ●N `. The word is the accessible name elsewhere.
#[must_use]
pub fn needs_you_chip(theme: &Theme, count: usize) -> Option<String> {
    (count > 0).then(|| format!(" {}{count} ", mark(theme, State::NeedsYou)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::theme::{Outer, Term};

    /// An agent of unknown state that asked wears the attention dot; a
    /// quiet one wears the dotted unknown mark, not a ring that reads as
    /// idle.
    #[test]
    fn an_unknown_agent_that_asked_wears_the_attention_dot() {
        let theme = Theme::default();
        let asked = agent_badge(&theme, AgentMetaState::Unknown, true, false);
        assert_eq!(asked, attention_badge(&theme));
        let quiet = agent_badge(&theme, AgentMetaState::Unknown, false, false);
        assert_eq!(quiet.glyph, "·");
        assert_eq!(quiet.color, theme.sidebar_section);
    }

    /// Every rung of the ladder has its own shape except the quiet pair
    /// (done-seen and idle), and the ASCII column is used whole.
    #[test]
    fn the_ladder_in_both_columns() {
        let unicode = Theme::default();
        let ascii = Theme {
            term: Term {
                glyphs: Glyphs::Ascii,
                ..unicode.term
            },
            ..unicode
        };
        let expect = [
            (State::NeedsYou, "●", "*"),
            (State::Failed, "✕", "x"),
            (State::Lost, "✕", "x"),
            (State::Done, "◆", "+"),
            (State::Working, "◐", "~"),
            (State::DoneSeen, "○", "o"),
            (State::Idle, "○", "o"),
            (State::Unknown, "·", "."),
            (State::Ended, "–", "-"),
        ];
        for (state, glyph, fallback) in expect {
            assert_eq!(mark(&unicode, state), glyph, "{state:?}");
            assert_eq!(mark(&ascii, state), fallback, "{state:?}");
            assert!(mark(&ascii, state).is_ascii());
        }
        let failed = pane_badge(
            &unicode,
            &ToneInput {
                agent: Some(Agent::Working),
                exit: Some(3),
                ..ToneInput::default()
            },
        );
        assert_eq!(
            (failed.glyph, failed.color, failed.emphatic),
            ("✕", unicode.agent_failed, true)
        );
        let ended = pane_badge(
            &unicode,
            &ToneInput {
                exit: Some(0),
                ..ToneInput::default()
            },
        );
        assert_eq!(ended.glyph, "–");
    }

    #[test]
    fn the_status_bar_chip_is_mark_and_count() {
        let theme = Theme::default();
        assert_eq!(needs_you_chip(&theme, 0), None);
        assert_eq!(needs_you_chip(&theme, 1).as_deref(), Some(" ●1 "));
        assert_eq!(needs_you_chip(&theme, 12).as_deref(), Some(" ●12 "));
        let ascii = Theme::house(&Outer {
            glyphs: Glyphs::Ascii,
            ..Outer::default()
        });
        assert_eq!(needs_you_chip(&ascii, 2).as_deref(), Some(" *2 "));
    }
}
