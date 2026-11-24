//! A work item's tile: who works in it and how that is going, at a glance.
//! An agent is a rounded square in its own colour with its initial; a plain
//! terminal is the prompt glyph on a quiet outline. A badge on the corner
//! carries the state, in its tone, only when there is one to tell.
//!
//! The sidebar's rows and the title bar's tabs both lead with it, so an
//! item reads the same wherever it is listed.

use phux_config_core::host_color::{PALETTE, auto_index};
use phux_design::State;
use rune::{Color, El, el, text};

use super::sidebar::{face, state_of};
use super::{Paint, Tokens, icons};
use crate::activity::Mark;
use crate::host_color;
use crate::ui::tokens::Voice;

/// Whether a badge tells anything: what needs you, failed, finished unseen,
/// is working, or rang. Idle and a shell's own command stay quiet.
pub(in crate::ui) fn tells(mark: Mark) -> bool {
    mark == Mark::Bell
        || (mark != Mark::None
            && matches!(
                state_of(mark),
                State::NeedsYou | State::Failed | State::Done | State::Working
            ))
}

/// The loudest mark among `marks` that a badge tells, if any.
pub(in crate::ui) fn loudest(marks: impl IntoIterator<Item = Mark>) -> Option<Mark> {
    marks
        .into_iter()
        .filter(|m| tells(*m))
        .min_by_key(|m| state_of(*m).rank())
}

/// The initial an agent's tile carries: its kind's first letter, as `C`
/// for `claude`.
fn initial(agent: &str) -> Option<String> {
    agent
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
}

/// The agent's colour: one of the identity palette's hashed hues, eased to
/// read on `ground`. The same agent is the same colour on every machine.
fn hue(agent: &str, ground: Color) -> Color {
    host_color::eased(PALETTE[auto_index(&agent.to_lowercase())].1, ground)
}

/// The tile: `agent` (its kind slug, else its name) or a plain terminal,
/// badged with `mark` when it tells. `ground` is what it sits on.
pub(in crate::ui) fn tile(agent: Option<&str>, mark: Mark, ground: Color, t: &Tokens) -> El {
    let size = t.space.s5;
    let face_el = match agent.and_then(|a| initial(a).map(|i| (a, i))) {
        Some((agent, letter)) => {
            let fill = hue(agent, ground);
            el().size(size, size)
                .radius(t.radius.sm)
                .continuous_corners()
                .center()
                .fill(fill)
                .child(
                    text(letter)
                        .font(t.ty.caption.semibold())
                        .color(crate::theme::on_fill(fill))
                        .a11y_hidden(),
                )
        }
        None => el()
            .size(size, size)
            .radius(t.radius.sm)
            .continuous_corners()
            .center()
            .border(t.space.hairline, t.roles.border.muted.paint())
            .child(
                rune::icon(&icons::PROMPT, t.icon.size.sm)
                    .color(t.roles.text.dim.paint())
                    .a11y_hidden(),
            ),
    };
    let badge = tells(mark).then(|| badge(mark, ground, t));
    el().size(size, size)
        .a11y_hidden()
        .child(face_el)
        .children(badge)
}

/// The corner badge: the state's own vector mark in its tone, so it reads
/// by shape as well as colour, on a disc of the ground so it reads off any
/// agent's colour. Never animated: a busy agent costs no frames.
fn badge(mark: Mark, ground: Color, t: &Tokens) -> El {
    let f = face(mark, t);
    let glyph = t.icon.size.xs;
    let disc = glyph + t.space.s0_5;
    let out = Some(-t.space.s1);
    // On the tile's bottom-right corner (`pin` is left, top, right, bottom).
    el().pin(None, None, out, out)
        .size(disc, disc)
        .radius(t.radius.full)
        .fill(ground)
        .center()
        .inert()
        .child(rune::icon(f.icon, glyph).color(f.ink))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_state_worth_telling_gets_a_badge() {
        for mark in [Mark::Blocked, Mark::Failed, Mark::Done, Mark::Working, Mark::Bell] {
            assert!(tells(mark), "{mark:?}");
        }
        for mark in [Mark::None, Mark::Idle] {
            assert!(!tells(mark), "{mark:?}");
        }
        assert_eq!(
            loudest([Mark::Working, Mark::None, Mark::Blocked, Mark::Done]),
            Some(Mark::Blocked)
        );
        assert_eq!(loudest([Mark::None, Mark::Idle]), None);
    }

    #[test]
    fn an_agent_keeps_its_colour_and_initial() {
        assert_eq!(initial("claude").as_deref(), Some("C"));
        assert_eq!(initial("-opencode").as_deref(), Some("O"));
        assert_eq!(initial("✳"), None);
        let ground = Color::BLACK;
        assert_eq!(hue("claude", ground), hue("Claude", ground));
    }
}
