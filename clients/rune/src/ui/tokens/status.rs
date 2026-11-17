//! Status ink (§1.3): each tone's colour and companions, and each ladder
//! state's mark, word and weight. Replaces the views' own mark colours.

use phux_design::{Roles, State, Tone};
use rune::{Color, Icon};

use super::Paint;
use crate::ui::icons;

/// One tone and its companions, ready to paint. The alpha is in each.
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub(crate) struct ToneColors {
    pub(crate) ink: Color,
    /// Row and badge fills.
    pub(crate) wash: Color,
    /// Borders.
    pub(crate) edge: Color,
    /// The only coloured shadow.
    pub(crate) glow: Color,
}

/// How one ladder state draws.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StateInk {
    pub(crate) tone: ToneColors,
    /// The vector mark (GUI surfaces never use the glyph).
    pub(crate) icon: &'static Icon,
    /// The word, also the accessible name where the word is hidden; none
    /// for unknown, which shows the provider's name.
    pub(crate) word: Option<&'static str>,
    pub(crate) bold: bool,
}

/// Every tone of the window's roles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StatusInk {
    tones: [ToneColors; 7],
    /// `ended` draws in `text.faint`.
    ended: Color,
}

const fn index(tone: Tone) -> usize {
    match tone {
        Tone::NeedsYou => 0,
        Tone::Working => 1,
        Tone::Done => 2,
        Tone::Failed => 3,
        Tone::Idle => 4,
        Tone::Unknown => 5,
        Tone::Bell => 6,
    }
}

impl StatusInk {
    pub(crate) fn new(roles: &Roles) -> Self {
        let tones = Tone::ALL.map(|tone| {
            let t = roles.tone(tone);
            ToneColors {
                ink: t.ink.paint(),
                wash: t.wash.paint(),
                edge: t.edge.paint(),
                glow: t.glow.paint(),
            }
        });
        Self {
            tones,
            ended: roles.text.faint.paint(),
        }
    }

    pub(crate) const fn tone(&self, tone: Tone) -> ToneColors {
        self.tones[index(tone)]
    }

    /// The ink, mark and word for `state`.
    pub(crate) fn state(&self, state: State) -> StateInk {
        let mut tone = self.tone(state.tone());
        if state == State::Ended {
            tone.ink = self.ended;
        }
        let mark = state.mark();
        StateInk {
            tone,
            icon: icon(state),
            word: state.word(),
            bold: mark.bold,
        }
    }
}

/// The vector mark for `state` (§1.3 rule 1).
pub(crate) const fn icon(state: State) -> &'static Icon {
    match state {
        State::NeedsYou => &icons::STATUS_NEEDS_YOU,
        State::Failed | State::Lost => &icons::STATUS_FAILED,
        State::Done => &icons::STATUS_DONE,
        State::Working => &icons::STATUS_WORKING,
        State::DoneSeen | State::Idle => &icons::STATUS_QUIET,
        State::Unknown => &icons::STATUS_UNKNOWN,
        State::Ended => &icons::STATUS_ENDED,
    }
}
