//! Chrome layer: status bar, dividers, sidebar. The badge vocabulary is
//! shared ([`agent_badge`]) so a pane reads the same on its title and its
//! sidebar row.

pub mod dividers;
pub mod sidebar;
pub mod sidebar_sections;
pub mod status_bar;

#[cfg(test)]
mod gallery_tests;

use phux_design::{Agent, State, Tone, ToneInput, tone_of};
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

/// The ladder column this process's terminal allows, as every theme built
/// here gets it ([`crate::render::theme::Outer::detect`]).
fn process_glyphs() -> Glyphs {
    static GLYPHS: std::sync::OnceLock<Glyphs> = std::sync::OnceLock::new();
    *GLYPHS.get_or_init(|| crate::render::theme::Outer::detect().glyphs)
}

/// The ladder mark for `state` in this terminal's glyph column, for text
/// drawn without a theme at hand (the fleet picker, onboarding hints): the
/// same column the sidebar uses, so marks never mix.
#[must_use]
pub(crate) fn glyph(state: State) -> &'static str {
    column(process_glyphs(), state)
}

/// The one glyph for a declared agent state, shared by the sidebar, the tab
/// bar, pane titles and the fleet picker, through [`phux_design::tone_of`]:
/// an unknown agent wears the dotted mark, not the idle ring.
#[must_use]
pub(crate) fn agent_state_glyph(state: AgentMetaState) -> &'static str {
    glyph(tone_of(&input(state, false, false)))
}

/// The vocabulary word for a declared agent state (`needs you`, never the
/// protocol's `blocked`); `None` for unknown, which shows the provider's
/// name instead.
#[must_use]
pub(crate) fn state_word(state: AgentMetaState) -> Option<&'static str> {
    tone_of(&input(state, false, false)).word()
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

/// The ladder mark for `state` in the theme's glyph column.
#[must_use]
pub fn mark(theme: &Theme, state: State) -> &'static str {
    column(theme.term.glyphs, state)
}

fn column(glyphs: Glyphs, state: State) -> &'static str {
    let row = state.mark();
    match glyphs {
        Glyphs::Unicode => row.glyph,
        Glyphs::Ascii => row.ascii,
    }
}

/// The slot a ladder state paints in: its tone's, so done-seen reads in the
/// idle tone here as in every other client.
#[must_use]
pub const fn state_color(theme: &Theme, state: State) -> Color {
    match state.tone() {
        Tone::NeedsYou => theme.agent_blocked,
        Tone::Failed => theme.agent_failed,
        Tone::Done => theme.agent_done,
        Tone::Working => theme.agent_working,
        Tone::Idle => theme.agent_idle,
        // `unknown` and `ended` draw in `text.faint`; no ladder state wears
        // the bell's tone.
        Tone::Unknown | Tone::Bell => theme.sidebar_section,
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
    state_badge(theme, tone_of(facts))
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

    /// A state paints in its tone's slot, so the TUI cannot drift from the
    /// other clients: done-seen reads in the idle tone, not done's green.
    #[test]
    fn a_state_paints_in_its_tones_slot() {
        let theme = Theme::default();
        for a in State::ALL {
            for b in State::ALL {
                if a.tone() == b.tone() {
                    assert_eq!(
                        state_color(&theme, a),
                        state_color(&theme, b),
                        "{a:?} {b:?}"
                    );
                }
            }
        }
        assert_eq!(state_color(&theme, State::DoneSeen), theme.agent_idle);
        assert_eq!(state_color(&theme, State::Done), theme.agent_done);
    }

    /// An agent of unknown state that asked wears the attention dot; a
    /// quiet one wears the dotted unknown mark, not a ring that reads as
    /// idle.
    #[test]
    fn an_unknown_agent_that_asked_wears_the_attention_dot() {
        let theme = Theme::default();
        let asked = agent_badge(&theme, AgentMetaState::Unknown, true, false);
        assert_eq!(asked, state_badge(&theme, State::NeedsYou));
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
