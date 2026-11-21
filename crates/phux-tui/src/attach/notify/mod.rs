//! Agent notifications, from the same agent rows the sidebar paints so a
//! notification and the badge agree. [`decide`] is the pure edge rule;
//! [`Notifier`] tracks, coalesces, cancels, and rate-limits; [`deliver`]
//! turns a [`Note`] into bytes or a desktop-notifier argv.

pub(super) mod deliver;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use phux_client::agent_meta::AgentMetaState;
use phux_config::{NotificationsCfg, NotifyMode};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::PaneNotify;
use tokio::time::Instant;

/// How long an edge waits before it posts: a flap back inside this window
/// cancels it, and edges that land inside it post as one.
pub(super) const COALESCE: Duration = Duration::from_secs(1);

/// The shortest gap between two posts of the same kind for the same pane.
pub(super) const RATE_LIMIT: Duration = Duration::from_secs(10);

/// The rate-limit bucket for notes, beside [`NotifyKind::bucket`]'s.
const NOTE_BUCKET: u8 = 2;

/// After a notifier is built, edges only re-baseline for this long, so an
/// attach or session switch never replays every already-blocked agent.
pub(super) const ATTACH_GRACE: Duration = Duration::from_secs(2);

/// Tracks beyond this are dropped wholesale; a long-lived client churning
/// through panes must not grow without bound.
const MAX_TRACKS: usize = 4096;

/// The coarse lifecycle a notification cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// No usable state yet.
    Unknown,
    /// Running work.
    Working,
    /// Waiting on a human: blocked, or an ADR-0035 ask.
    Blocked,
    /// Done or idle.
    Settled,
}

impl Phase {
    /// Fold an agent row's state and attention bit; attention wins, as it
    /// does on the attention ladder.
    pub(super) const fn of(state: AgentMetaState, attention: bool) -> Self {
        if attention {
            return Self::Blocked;
        }
        match state {
            AgentMetaState::Blocked => Self::Blocked,
            AgentMetaState::Working => Self::Working,
            AgentMetaState::Done | AgentMetaState::Idle => Self::Settled,
            AgentMetaState::Unknown => Self::Unknown,
        }
    }
}

/// One agent-running pane, as the notifier sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NotifyRow {
    /// The pane's identity, stable across layout edits and session switches.
    pub key: ResourceId,
    /// Agent display name.
    pub name: String,
    /// Folded lifecycle.
    pub phase: Phase,
    /// Blocked because the agent asked (not because its state says blocked).
    pub asking: bool,
    /// The session holding the pane.
    pub session: String,
    /// Where in the session: the window name or the pane's cwd.
    pub place: String,
    /// Satellite host, when the agent runs elsewhere.
    pub host: Option<String>,
    /// The pane is the focused pane of the attached session.
    pub viewing: bool,
}

/// What [`decide`] remembers about one pane between observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Track {
    /// The last observed phase.
    pub phase: Phase,
    /// The agent has worked since its last finish; arms the done edge.
    pub worked: bool,
    /// The pane was the focused pane at the last observation.
    pub viewing: bool,
}

/// Which edge fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum NotifyKind {
    /// Into blocked; `asking` when an ask, not the state, put it there.
    Blocked {
        /// The agent raised an ADR-0035 ask.
        asking: bool,
    },
    /// Working, then done or idle.
    Done,
}

impl NotifyKind {
    /// Rate-limit bucket: an ask and a blocked state are the same news.
    const fn bucket(self) -> u8 {
        match self {
            Self::Blocked { .. } => 0,
            Self::Done => 1,
        }
    }

    /// The phase a pending post of this kind still describes.
    const fn holds_in(self, phase: Phase) -> bool {
        matches!(
            (self, phase),
            (Self::Blocked { .. }, Phase::Blocked) | (Self::Done, Phase::Settled)
        )
    }

    const fn enabled(self, cfg: NotificationsCfg) -> bool {
        match self {
            Self::Blocked { .. } => cfg.blocked,
            Self::Done => cfg.done,
        }
    }
}

/// The pure edge rule: prior track and new row to the next track and the
/// edge to post. Into [`Phase::Blocked`] fires (a first sighting only once
/// `primed`); into [`Phase::Settled`] fires once per run of work. Nothing
/// fires with `mode = "off"`, a disabled kind, or (with `suppress-focused`)
/// for the focused pane.
pub(super) fn decide(
    prior: Option<Track>,
    row: &NotifyRow,
    client_focused: bool,
    primed: bool,
    cfg: NotificationsCfg,
) -> (Track, Option<NotifyKind>) {
    let worked = prior.is_some_and(|track| track.worked);
    let mut next = Track {
        phase: row.phase,
        worked,
        viewing: row.viewing,
    };
    let edge = match row.phase {
        Phase::Working => {
            next.worked = true;
            None
        }
        Phase::Blocked => {
            let entered = prior.map_or(primed, |track| track.phase != Phase::Blocked);
            entered.then_some(NotifyKind::Blocked { asking: row.asking })
        }
        Phase::Settled => {
            next.worked = false;
            worked.then_some(NotifyKind::Done)
        }
        Phase::Unknown => None,
    };
    let looking = cfg.suppress_focused && row.viewing && client_focused;
    let edge = edge.filter(|kind| cfg.mode != NotifyMode::Off && kind.enabled(cfg) && !looking);
    (next, edge)
}

/// A notification ready for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Note {
    /// One line: who and what.
    pub title: String,
    /// One line: where.
    pub body: String,
}

/// Build the note for one edge.
pub(super) fn describe(row: &NotifyRow, kind: NotifyKind) -> Note {
    let title = format!("{} {}", row.name, headline(kind));
    let mut body = if row.place.is_empty() || row.place == row.session {
        row.session.clone()
    } else {
        format!("{} in {}", row.place, row.session)
    };
    if let Some(host) = row.host.as_deref().filter(|host| !host.is_empty()) {
        body.push_str(" on ");
        body.push_str(host);
    }
    Note { title, body }
}

/// What happened, in the status vocabulary's words: an agent asking and
/// one held for approval both need you.
fn headline(kind: NotifyKind) -> &'static str {
    let state = match kind {
        NotifyKind::Blocked { .. } => phux_design::State::NeedsYou,
        NotifyKind::Done => phux_design::State::Done,
    };
    state.word().unwrap_or_default()
}

/// Fold several due posts into one note.
fn summarize(posts: &[Pending]) -> Option<Note> {
    match posts {
        [] => None,
        [one] => Some(one.note.clone()),
        many => {
            let blocked = many
                .iter()
                .any(|post| matches!(post.kind, NotifyKind::Blocked { .. }));
            let key = if blocked {
                "count.needs_you"
            } else {
                "count.done"
            };
            let title = phux_design::plural(key, many.len() as u64).unwrap_or_default();
            let body = many
                .iter()
                .map(|post| format!("{} {}", post.name, headline(post.kind)))
                .collect::<Vec<_>>()
                .join(", ");
            Some(Note { title, body })
        }
    }
}

/// An edge waiting out [`COALESCE`].
#[derive(Debug, Clone)]
struct Pending {
    due: Instant,
    key: ResourceId,
    kind: NotifyKind,
    name: String,
    note: Note,
}

/// Per-client notification state for one attach entry.
#[derive(Debug)]
pub(super) struct Notifier {
    tracks: HashMap<ResourceId, Track>,
    pending: Vec<Pending>,
    last_sent: HashMap<(ResourceId, u8), Instant>,
    primed_at: Instant,
    /// The outer terminal window's focus, from `?1004` reports. A terminal
    /// that never reports counts as focused.
    client_focused: bool,
    /// Panes whose `phux.notify/v1` mode is not `on`; they never post.
    muted: HashMap<ResourceId, PaneNotify>,
    /// Panes whose mode is watched, and the reads still in flight.
    pub(in crate::attach) mode_watched: HashSet<ResourceId>,
    pub(in crate::attach) mode_reads: HashMap<u32, ResourceId>,
}

impl Notifier {
    /// A notifier whose first sightings re-baseline until [`ATTACH_GRACE`]
    /// has passed.
    pub(super) fn new(now: Instant) -> Self {
        Self {
            tracks: HashMap::new(),
            pending: Vec::new(),
            last_sent: HashMap::new(),
            primed_at: now + ATTACH_GRACE,
            client_focused: true,
            muted: HashMap::new(),
            mode_watched: HashSet::new(),
            mode_reads: HashMap::new(),
        }
    }

    /// Fold a pane's `phux.notify/v1` mode; muting drops what it queued.
    pub(in crate::attach) fn set_mode(&mut self, key: ResourceId, mode: PaneNotify) {
        if mode.alerts() {
            self.muted.remove(&key);
        } else {
            self.pending.retain(|post| post.key != key);
            self.muted.insert(key, mode);
        }
    }

    /// Whether `key` may post: its mode is `on`.
    pub(in crate::attach) fn alerts(&self, key: &ResourceId) -> bool {
        !self.muted.contains_key(key)
    }

    /// Fold an outer-terminal focus report.
    pub(super) const fn set_client_focused(&mut self, focused: bool) {
        self.client_focused = focused;
    }

    /// Fold one projection of the agent list. Panes absent from `rows` keep
    /// their track: absence is a projection gap, not a death.
    pub(super) fn observe(&mut self, rows: &[NotifyRow], cfg: NotificationsCfg, now: Instant) {
        if self.tracks.len() > MAX_TRACKS {
            self.tracks.clear();
        }
        let primed = now >= self.primed_at;
        for row in rows {
            let prior = self.tracks.get(&row.key).copied();
            let (track, edge) = decide(prior, row, self.client_focused, primed, cfg);
            self.tracks.insert(row.key.clone(), track);
            let Some(kind) = edge.filter(|_| self.alerts(&row.key)) else {
                continue;
            };
            let queued = self
                .pending
                .iter()
                .any(|post| post.key == row.key && post.kind.bucket() == kind.bucket());
            if !queued {
                self.pending.push(Pending {
                    due: now + COALESCE,
                    key: row.key.clone(),
                    kind,
                    name: row.name.clone(),
                    note: describe(row, kind),
                });
            }
        }
        self.drop_stale_pending(cfg);
    }

    /// Drop pending posts the pane no longer supports (it flapped back) or
    /// that the user has since looked at.
    fn drop_stale_pending(&mut self, cfg: NotificationsCfg) {
        let client_focused = self.client_focused;
        let tracks = &self.tracks;
        self.pending.retain(|post| {
            tracks.get(&post.key).is_none_or(|track| {
                post.kind.holds_in(track.phase)
                    && !(cfg.suppress_focused && track.viewing && client_focused)
            })
        });
    }

    /// Whether a warn or error note for `key` notifies now: not with
    /// `mode = "off"` or a muted pane, not (with `suppress-focused`) while
    /// the pane is `seen` in a focused window, and at most once per
    /// [`RATE_LIMIT`].
    pub(super) fn note_due(
        &mut self,
        key: &ResourceId,
        seen: bool,
        cfg: NotificationsCfg,
        now: Instant,
    ) -> bool {
        let looking = cfg.suppress_focused && seen && self.client_focused;
        if cfg.mode == NotifyMode::Off || looking || !self.alerts(key) {
            return false;
        }
        let slot = (key.clone(), NOTE_BUCKET);
        if self
            .last_sent
            .get(&slot)
            .is_some_and(|sent| now.saturating_duration_since(*sent) < RATE_LIMIT)
        {
            return false;
        }
        self.last_sent.insert(slot, now);
        true
    }

    /// When the earliest pending post is due.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.pending.iter().map(|post| post.due).min()
    }

    /// Take every post due at `now`, rate-limited per pane and kind, folded
    /// into at most one note.
    pub(super) fn take_due(&mut self, cfg: NotificationsCfg, now: Instant) -> Option<Note> {
        self.drop_stale_pending(cfg);
        let (due, later): (Vec<_>, Vec<_>) =
            self.pending.drain(..).partition(|post| post.due <= now);
        self.pending = later;
        let mut posts = Vec::with_capacity(due.len());
        for post in due {
            let slot = (post.key.clone(), post.kind.bucket());
            if self
                .last_sent
                .get(&slot)
                .is_some_and(|sent| now.saturating_duration_since(*sent) < RATE_LIMIT)
            {
                continue;
            }
            self.last_sent.insert(slot, now);
            posts.push(post);
        }
        if cfg.mode == NotifyMode::Off {
            return None;
        }
        summarize(&posts)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
