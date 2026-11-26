//! How a terminal's state draws: its ladder state, tone, vector
//! mark and word, the counts a session shows, and the type weights the
//! chrome's names use.

use phux_design::{State, Tone};
use rune::{Color, El, Icon, row};

use super::super::{Tokens, icons};
use crate::activity::Mark;
use crate::ui::tokens::Voice;

/// The ladder state a terminal's mark stands for. A shell's own
/// command counts as work under way; a bell asks for you, as the pane's
/// attention flag does.
pub(in crate::ui) const fn state_of(mark: Mark) -> State {
    match mark.state() {
        Some(state) => state,
        None => State::NeedsYou,
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

/// A mark's colour: its tone's, never a decoration.
pub(in crate::ui) fn mark_color(mark: Mark, ink: &Tokens) -> Color {
    face(mark, ink).ink
}

/// A small identity dot: a session's or machine's colour.
pub(in crate::ui) fn dot_size(t: &Tokens) -> f32 {
    t.space.s2 - t.space.s0_5
}

/// What a session's terminals ask of you, loudest first:
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

/// A quiet unboxed summary (`● 2  ✕ 1  ◆ 3`), preserving tone, shape and count.
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
                    let rung = t.status.state(state);
                    row()
                        .gap(t.space.s1)
                        .items_center()
                        .label(said)
                        .child(
                            rune::icon(rung.icon, t.icon.size.xs)
                                .color(rung.tone.ink)
                                .a11y_hidden(),
                        )
                        .child(
                            rune::text(n.to_string())
                                .font(t.ty.caption.semibold())
                                .color(rung.tone.ink)
                                .a11y_hidden(),
                        )
                }),
        )
}
