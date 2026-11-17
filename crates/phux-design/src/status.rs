//! The status vocabulary (spec §1.3): one mapping from pane facts to a
//! ladder state, the state's tone, mark and word, and English plurals.

use crate::tokens::{self, StatusMark};

/// The agent state a pane reports (`AgentMetaState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Agent {
    Working,
    Blocked,
    Idle,
    Done,
    Unknown,
}

/// The transport to the pane's host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Link {
    Ok,
    Lost,
}

/// What a client knows about a pane (§1.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the independent pane facts of §1.3.1"
)]
pub struct ToneInput {
    pub agent: Option<Agent>,
    pub attention: bool,
    pub asked: bool,
    /// The pane's exit code, once it exited.
    pub exit: Option<i32>,
    pub link: Option<Link>,
    pub crashed: bool,
    /// Visited since it finished (client-local).
    pub seen: bool,
}

/// A rung of the status ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    NeedsYou,
    Failed,
    /// Failed because the transport is gone: the same tone, its own word.
    Lost,
    /// Done and not yet visited.
    Done,
    /// Done and visited: the quiet class, beside idle.
    DoneSeen,
    Working,
    Idle,
    Unknown,
    /// Exited cleanly with no agent.
    Ended,
}

/// The colour a state is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    NeedsYou,
    Working,
    Done,
    Failed,
    Idle,
    Unknown,
    Bell,
}

impl Tone {
    /// Every tone, in the token order.
    pub const ALL: [Self; 7] = [
        Self::NeedsYou,
        Self::Working,
        Self::Done,
        Self::Failed,
        Self::Idle,
        Self::Unknown,
        Self::Bell,
    ];

    /// The role key (`status.<key>`).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs_you",
            Self::Working => "working",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
            Self::Bell => "bell",
        }
    }
}

impl State {
    /// Every state, in ladder (rank) order.
    pub const ALL: [Self; 9] = [
        Self::NeedsYou,
        Self::Failed,
        Self::Lost,
        Self::Done,
        Self::Working,
        Self::DoneSeen,
        Self::Idle,
        Self::Unknown,
        Self::Ended,
    ];

    /// The vectors' and the ladder's key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs_you",
            Self::Failed => "failed",
            Self::Lost => "lost",
            Self::Done => "done",
            Self::DoneSeen => "done_seen",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
            Self::Ended => "ended",
        }
    }

    /// The state's tone. `Ended` draws in `text.faint`, which is the
    /// `unknown` tone's colour.
    #[must_use]
    pub const fn tone(self) -> Tone {
        match self {
            Self::NeedsYou => Tone::NeedsYou,
            Self::Failed | Self::Lost => Tone::Failed,
            Self::Done => Tone::Done,
            Self::Working => Tone::Working,
            Self::DoneSeen | Self::Idle => Tone::Idle,
            Self::Unknown | Self::Ended => Tone::Unknown,
        }
    }

    /// The ladder row: glyph, ASCII fallback, vector icon, rank and weight.
    #[must_use]
    pub fn mark(self) -> &'static StatusMark {
        let key = self.key();
        tokens::STATUS
            .iter()
            .find(|m| m.state == key)
            .unwrap_or(&tokens::STATUS[0])
    }

    /// The state's word; `None` for unknown, which shows the provider's name.
    #[must_use]
    pub const fn word(self) -> Option<&'static str> {
        let w = &tokens::VOCABULARY.words;
        Some(match self {
            Self::NeedsYou => w.needs_you,
            Self::Failed => w.failed,
            Self::Lost => w.lost,
            Self::Done | Self::DoneSeen => w.done,
            Self::Working => w.working,
            Self::Idle => w.idle,
            Self::Ended => w.ended,
            Self::Unknown => return None,
        })
    }

    /// Rank 1 is the loudest; a session shows its panes' lowest rank.
    #[must_use]
    pub fn rank(self) -> u32 {
        self.mark().rank
    }
}

/// The one mapping from pane facts to a ladder state.
///
/// A pane whose transport is gone is `lost`; a dead pane (crashed or nonzero exit) is
/// `failed`; a clean exit is `done` with an agent and `ended` without one;
/// only a live pane can need you.
#[must_use]
pub fn tone_of(i: &ToneInput) -> State {
    let agent = i.agent.unwrap_or(Agent::Unknown);
    let finished = |seen: bool| if seen { State::DoneSeen } else { State::Done };
    if i.link == Some(Link::Lost) {
        return State::Lost;
    }
    if i.crashed || i.exit.is_some_and(|code| code != 0) {
        return State::Failed;
    }
    if i.exit == Some(0) {
        return if agent == Agent::Unknown {
            State::Ended
        } else {
            finished(i.seen)
        };
    }
    if agent == Agent::Blocked || i.attention || i.asked {
        return State::NeedsYou;
    }
    match agent {
        Agent::Done => finished(i.seen),
        Agent::Working => State::Working,
        Agent::Idle => State::Idle,
        Agent::Blocked | Agent::Unknown => State::Unknown,
    }
}

/// A status word by its vocabulary key (`needs_you`, `failed`, …).
#[must_use]
pub fn word(key: &str) -> Option<&'static str> {
    let w = &tokens::VOCABULARY.words;
    Some(match key {
        "needs_you" => w.needs_you,
        "working" => w.working,
        "done" => w.done,
        "failed" => w.failed,
        "lost" => w.lost,
        "idle" => w.idle,
        "ended" => w.ended,
        "bell" => w.bell,
        _ => return None,
    })
}

/// `count` in the vocabulary's plural `key` (`count.needs_you`, …):
/// "1 needs you", "2 need you". `None` for a key the vocabulary lacks.
#[must_use]
pub fn plural(key: &str, count: u64) -> Option<String> {
    let c = &tokens::VOCABULARY.plurals.count;
    let message = match key {
        "count.needs_you" => c.needs_you,
        "count.failed" => c.failed,
        "count.done" => c.done,
        _ => return None,
    };
    plural_message(message, count)
}

/// An ICU `{count, plural, …}` message with English CLDR categories: an
/// exact `=N` form first, then `one` for exactly 1, then `other`.
#[must_use]
pub fn plural_message(message: &str, count: u64) -> Option<String> {
    let body = message
        .strip_prefix("{count, plural, ")?
        .strip_suffix('}')?;
    let mut forms: Vec<(&str, &str)> = Vec::new();
    let mut rest = body.trim_start();
    while !rest.is_empty() {
        let open = rest.find('{')?;
        let close = open + rest[open..].find('}')?;
        forms.push((rest[..open].trim(), &rest[open + 1..close]));
        rest = rest[close + 1..].trim_start();
    }
    let find = |k: &str| forms.iter().find(|(name, _)| *name == k).map(|(_, f)| *f);
    let exact = format!("={count}");
    let form = find(&exact)
        .or_else(|| (count == 1).then(|| find("one")).flatten())
        .or_else(|| find("other"))?;
    Some(form.replace('#', &count.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quiet_class_shares_a_mark_but_not_a_word() {
        assert_eq!(State::DoneSeen.mark().glyph, State::Idle.mark().glyph);
        assert_ne!(State::DoneSeen.word(), State::Idle.word());
    }

    #[test]
    fn every_state_has_a_ladder_row() {
        for s in State::ALL {
            assert_eq!(s.mark().state, s.key());
        }
        assert_eq!(State::Unknown.word(), None);
        assert_eq!(word("needs_you"), Some("needs you"));
    }

    #[test]
    fn plurals_cover_zero_one_and_many() {
        assert_eq!(plural("count.needs_you", 0).as_deref(), Some("0 need you"));
        assert_eq!(plural("count.needs_you", 1).as_deref(), Some("1 needs you"));
        assert_eq!(plural("count.needs_you", 7).as_deref(), Some("7 need you"));
        assert_eq!(plural("count.nope", 1), None);
        assert_eq!(
            plural_message("{count, plural, =0 {none} one {# x} other {# xs}}", 0).as_deref(),
            Some("none")
        );
    }
}
