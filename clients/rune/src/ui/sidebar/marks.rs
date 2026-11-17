//! How a terminal's state draws (§1.3): its ladder state, tone, vector
//! mark and word, the counts a session shows, and the type weights the
//! chrome's names use.

use phux_design::{State, Tone};
use rune::{Color, El, Icon, TextStyle, el, row};

use super::super::{Paint, Tokens, badge, icons};
use crate::activity::Mark;
use crate::host_color;

/// The ladder state a terminal's mark stands for (§1.3). A shell's own
/// command counts as work under way; a bell asks for you, as the pane's
/// attention flag does.
pub(in crate::ui) const fn state_of(mark: Mark) -> State {
    match mark {
        Mark::Blocked | Mark::Bell => State::NeedsYou,
        Mark::Failed => State::Failed,
        Mark::Done => State::Done,
        Mark::Working | Mark::Running | Mark::Background => State::Working,
        Mark::Succeeded => State::DoneSeen,
        Mark::Idle => State::Idle,
        Mark::None => State::Unknown,
    }
}

/// How a mark draws: its tone's ink, its vector mark and its word. A bell
/// keeps its own mark and tone.
pub(in crate::ui) struct Face {
    pub(in crate::ui) ink: Color,
    pub(in crate::ui) icon: &'static Icon,
    pub(in crate::ui) word: Option<&'static str>,
    pub(in crate::ui) bold: bool,
}

pub(in crate::ui) fn face(mark: Mark, t: &Tokens) -> Face {
    if mark == Mark::Bell {
        return Face {
            ink: t.status.tone(Tone::Bell).ink,
            icon: &icons::BELL,
            word: phux_design::word("bell"),
            bold: false,
        };
    }
    let s = t.status.state(state_of(mark));
    Face {
        ink: s.tone.ink,
        icon: s.icon,
        word: s.word,
        bold: s.bold,
    }
}

/// A mark's colour: its tone's (§1.3), never a decoration.
pub(in crate::ui) fn mark_color(mark: Mark, ink: &Tokens) -> Color {
    face(mark, ink).ink
}

/// `style` at one of the type scale's weights.
pub(in crate::ui) fn weighted(style: TextStyle, weight: u32) -> TextStyle {
    TextStyle {
        weight: f32::from(u16::try_from(weight).unwrap_or(400)),
        ..style
    }
}

/// The label role, a step heavier: names that lead a group.
pub(in crate::ui) fn strong(t: &Tokens) -> TextStyle {
    weighted(t.ty.label, phux_design::tokens::TYPE.weights.semibold)
}

/// A small identity dot: a session's or machine's colour.
pub(in crate::ui) fn dot_size(t: &Tokens) -> f32 {
    t.space.s2 - t.space.s0_5
}

/// The leading mark: the state's vector mark in its tone, never animated,
/// so a busy agent costs no frames. A plain shell with a session colour
/// shows that colour instead; without one, the quiet dotted ring.
pub(super) fn lead(mark: Mark, session: Option<u32>, t: &Tokens) -> El {
    let mark = match (mark, session) {
        (Mark::None, Some(rgb)) => {
            host_color::mark(rgb, dot_size(t), t.roles.layer.sidebar.paint())
        }
        _ => {
            let f = face(mark, t);
            let icon = rune::icon(f.icon, t.icon.size.sm).color(f.ink);
            match f.word {
                Some(word) => icon.label(word),
                None => icon.a11y_hidden(),
            }
        }
    };
    el().size(t.icon.size.md, t.icon.size.md)
        .center()
        .child(mark)
}

/// What a session's terminals ask of you, loudest first (§1.3 rule 5):
/// how many need you, failed and finished unseen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(in crate::ui) struct Counts {
    pub(in crate::ui) needs_you: usize,
    pub(in crate::ui) failed: usize,
    pub(in crate::ui) done: usize,
}

impl Counts {
    pub(super) fn add(&mut self, mark: Mark) {
        match state_of(mark) {
            State::NeedsYou => self.needs_you += 1,
            State::Failed => self.failed += 1,
            State::Done => self.done += 1,
            _ => {}
        }
    }

    /// Everything here that asks for a person.
    pub(in crate::ui) const fn waiting(self) -> usize {
        self.needs_you + self.failed + self.done
    }
}

/// The histogram (`● 2  ✕ 1  ◆ 3`): one tone badge per rung with a count,
/// each named by its plural for a screen reader.
pub(in crate::ui) fn histogram(counts: Counts, t: &Tokens) -> El {
    let rungs = [
        (State::NeedsYou, counts.needs_you, "count.needs_you"),
        (State::Failed, counts.failed, "count.failed"),
        (State::Done, counts.done, "count.done"),
    ];
    row()
        .gap(t.space.s1)
        .items_center()
        .children(
            rungs
                .into_iter()
                .filter(|(_, n, _)| *n > 0)
                .map(|(state, n, key)| {
                    let said = phux_design::plural(key, u64::try_from(n).unwrap_or(u64::MAX))
                        .unwrap_or_else(|| n.to_string());
                    badge::tone_badge(t, state, Some(n.to_string())).label(said)
                }),
        )
}
