//! Agent notifications: the edges worth interrupting a human for.
//!
//! The input is the same agent list the sidebar's Agents zone paints (one
//! [`NotifyRow`] per agent-running pane, local and peer), so a notification
//! and the `!` badge always agree. [`decide`] is the pure edge rule; the
//! [`Notifier`] holds the per-pane track, coalesces a burst into one post,
//! cancels a pending post the user already looked at, and rate-limits a
//! flapping agent. [`deliver`] turns a [`Note`] into bytes for the outer
//! terminal or an argv for the desktop notifier.

pub(super) mod deliver;

use std::collections::HashMap;
use std::time::Duration;

use phux_client::agent_meta::AgentMetaState;
use phux_config::{NotificationsCfg, NotifyMode};
use phux_protocol::ids::ResourceId;
use tokio::time::Instant;

/// How long an edge waits before it posts: a flap back inside this window
/// cancels it, and edges that land inside it post as one.
pub(super) const COALESCE: Duration = Duration::from_secs(1);

/// The shortest gap between two posts of the same kind for the same pane.
pub(super) const RATE_LIMIT: Duration = Duration::from_secs(10);

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

/// The pure edge rule: prior track, the new row, whether the outer terminal
/// window has focus, whether first sightings may fire, and the config, to the
/// next track and the edge to post (if any).
///
/// - Into [`Phase::Blocked`] from anything else fires a blocked edge. A first
///   sighting fires only once `primed` (past the attach grace).
/// - Into [`Phase::Settled`] fires a done edge once per run: only when the
///   agent worked since its last finish.
/// - Nothing fires with `mode = "off"`, for a disabled kind, or (with
///   `suppress-focused`) for the pane the user is looking at.
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

/// What happened, in a notification's words.
const fn headline(kind: NotifyKind) -> &'static str {
    match kind {
        NotifyKind::Blocked { asking: true } => "is asking for you",
        NotifyKind::Blocked { asking: false } => "needs approval",
        NotifyKind::Done => "finished",
    }
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
            let title = if blocked {
                format!("{} agents need you", many.len())
            } else {
                format!("{} agents finished", many.len())
            };
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
        }
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
            let Some(kind) = edge else { continue };
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
mod tests {
    use super::*;

    fn cfg() -> NotificationsCfg {
        NotificationsCfg::default()
    }

    fn row(id: u32, phase: Phase) -> NotifyRow {
        NotifyRow {
            key: ResourceId::local(id),
            name: "claude".to_owned(),
            phase,
            asking: false,
            session: "dev".to_owned(),
            place: "api".to_owned(),
            host: None,
            viewing: false,
        }
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "reads as the `prior` argument at every call site"
    )]
    fn track(phase: Phase, worked: bool) -> Option<Track> {
        Some(Track {
            phase,
            worked,
            viewing: false,
        })
    }

    #[test]
    fn working_to_blocked_fires_blocked() {
        let (next, edge) = decide(
            track(Phase::Working, true),
            &row(1, Phase::Blocked),
            true,
            true,
            cfg(),
        );
        assert_eq!(edge, Some(NotifyKind::Blocked { asking: false }));
        assert!(next.worked, "a blocked run still finishes later");
    }

    #[test]
    fn staying_blocked_does_not_refire() {
        let (_, edge) = decide(
            track(Phase::Blocked, true),
            &row(1, Phase::Blocked),
            true,
            true,
            cfg(),
        );
        assert_eq!(edge, None);
    }

    #[test]
    fn done_fires_once_per_run() {
        let (next, edge) = decide(
            track(Phase::Working, true),
            &row(1, Phase::Settled),
            true,
            true,
            cfg(),
        );
        assert_eq!(edge, Some(NotifyKind::Done));
        assert!(!next.worked);
        let (_, again) = decide(Some(next), &row(1, Phase::Settled), true, true, cfg());
        assert_eq!(again, None, "a second identical done is not a new run");
    }

    #[test]
    fn blocked_then_done_after_working_fires_done() {
        let (t, _) = decide(
            track(Phase::Working, true),
            &row(1, Phase::Blocked),
            true,
            true,
            cfg(),
        );
        let (_, edge) = decide(Some(t), &row(1, Phase::Settled), true, true, cfg());
        assert_eq!(edge, Some(NotifyKind::Done));
    }

    #[test]
    fn idle_without_work_is_silent() {
        let (_, edge) = decide(None, &row(1, Phase::Settled), true, true, cfg());
        assert_eq!(edge, None);
        let (_, edge) = decide(
            track(Phase::Unknown, false),
            &row(1, Phase::Settled),
            true,
            true,
            cfg(),
        );
        assert_eq!(edge, None);
    }

    #[test]
    fn first_sighting_blocked_fires_only_when_primed() {
        let (_, early) = decide(None, &row(1, Phase::Blocked), true, false, cfg());
        assert_eq!(early, None);
        let (_, late) = decide(None, &row(1, Phase::Blocked), true, true, cfg());
        assert_eq!(late, Some(NotifyKind::Blocked { asking: false }));
    }

    #[test]
    fn ask_names_itself() {
        let mut asked = row(1, Phase::Blocked);
        asked.asking = true;
        let (_, edge) = decide(track(Phase::Working, true), &asked, true, true, cfg());
        assert_eq!(edge, Some(NotifyKind::Blocked { asking: true }));
    }

    #[test]
    fn focused_pane_in_focused_window_is_suppressed() {
        let mut viewing = row(1, Phase::Settled);
        viewing.viewing = true;
        let prior = track(Phase::Working, true);
        assert_eq!(decide(prior, &viewing, true, true, cfg()).1, None);
        // The terminal window lost focus: the user is not looking.
        assert_eq!(
            decide(prior, &viewing, false, true, cfg()).1,
            Some(NotifyKind::Done)
        );
        let loud = NotificationsCfg {
            suppress_focused: false,
            ..cfg()
        };
        assert_eq!(
            decide(prior, &viewing, true, true, loud).1,
            Some(NotifyKind::Done)
        );
    }

    #[test]
    fn mode_off_and_kind_toggles_silence_but_still_track() {
        let off = NotificationsCfg {
            mode: NotifyMode::Off,
            ..cfg()
        };
        let (next, edge) = decide(
            track(Phase::Working, true),
            &row(1, Phase::Settled),
            true,
            true,
            off,
        );
        assert_eq!(edge, None);
        assert!(!next.worked, "the run is consumed even when silent");
        let no_done = NotificationsCfg {
            done: false,
            ..cfg()
        };
        assert_eq!(
            decide(
                track(Phase::Working, true),
                &row(1, Phase::Settled),
                true,
                true,
                no_done
            )
            .1,
            None
        );
        let no_blocked = NotificationsCfg {
            blocked: false,
            ..cfg()
        };
        assert_eq!(
            decide(
                track(Phase::Working, true),
                &row(1, Phase::Blocked),
                true,
                true,
                no_blocked
            )
            .1,
            None
        );
    }

    #[test]
    fn phase_folds_attention_over_state() {
        assert_eq!(Phase::of(AgentMetaState::Idle, true), Phase::Blocked);
        assert_eq!(Phase::of(AgentMetaState::Done, false), Phase::Settled);
        assert_eq!(Phase::of(AgentMetaState::Idle, false), Phase::Settled);
        assert_eq!(Phase::of(AgentMetaState::Working, false), Phase::Working);
        assert_eq!(Phase::of(AgentMetaState::Unknown, false), Phase::Unknown);
    }

    #[test]
    fn describe_names_agent_place_session_and_host() {
        let mut r = row(1, Phase::Blocked);
        r.host = Some("buildbox".to_owned());
        let note = describe(&r, NotifyKind::Blocked { asking: false });
        assert_eq!(note.title, "claude needs approval");
        assert_eq!(note.body, "api in dev on buildbox");
        let done = describe(&row(1, Phase::Settled), NotifyKind::Done);
        assert_eq!(done.title, "claude finished");
        assert_eq!(done.body, "api in dev");
    }

    /// Prime a notifier and walk one pane to working.
    fn primed_working(now: Instant) -> Notifier {
        let mut n = Notifier::new(now);
        n.observe(&[row(1, Phase::Working)], cfg(), now + ATTACH_GRACE);
        n
    }

    #[test]
    fn edge_waits_out_the_coalesce_window() {
        let t0 = Instant::now();
        let mut n = primed_working(t0);
        let t1 = t0 + ATTACH_GRACE;
        n.observe(&[row(1, Phase::Settled)], cfg(), t1);
        assert_eq!(n.deadline(), Some(t1 + COALESCE));
        assert_eq!(n.take_due(cfg(), t1), None, "not due yet");
        let note = n.take_due(cfg(), t1 + COALESCE).expect("due");
        assert_eq!(note.title, "claude finished");
        assert_eq!(n.deadline(), None);
    }

    #[test]
    fn a_flap_back_inside_the_window_cancels() {
        let t0 = Instant::now();
        let mut n = primed_working(t0);
        let t1 = t0 + ATTACH_GRACE;
        n.observe(&[row(1, Phase::Settled)], cfg(), t1);
        n.observe(&[row(1, Phase::Working)], cfg(), t1 + COALESCE / 2);
        assert_eq!(n.take_due(cfg(), t1 + COALESCE), None);
    }

    #[test]
    fn viewing_the_pane_before_the_post_cancels_it() {
        let t0 = Instant::now();
        let mut n = primed_working(t0);
        let t1 = t0 + ATTACH_GRACE;
        n.observe(&[row(1, Phase::Settled)], cfg(), t1);
        let mut looked = row(1, Phase::Settled);
        looked.viewing = true;
        n.observe(&[looked], cfg(), t1 + COALESCE / 2);
        assert_eq!(n.take_due(cfg(), t1 + COALESCE), None);
    }

    #[test]
    fn a_burst_posts_as_one_summary() {
        let t0 = Instant::now();
        let mut n = Notifier::new(t0);
        let t1 = t0 + ATTACH_GRACE;
        n.observe(&[row(1, Phase::Working), row(2, Phase::Working)], cfg(), t1);
        let mut other = row(2, Phase::Blocked);
        other.name = "codex".to_owned();
        n.observe(&[row(1, Phase::Settled), other], cfg(), t1);
        let note = n.take_due(cfg(), t1 + COALESCE).expect("due");
        assert_eq!(note.title, "2 agents need you");
        assert_eq!(note.body, "claude finished, codex needs approval");
    }

    #[test]
    fn a_flapping_agent_is_rate_limited() {
        let t0 = Instant::now();
        let mut n = primed_working(t0);
        let mut t = t0 + ATTACH_GRACE;
        n.observe(&[row(1, Phase::Settled)], cfg(), t);
        t += COALESCE;
        assert!(n.take_due(cfg(), t).is_some());
        n.observe(&[row(1, Phase::Working)], cfg(), t);
        n.observe(&[row(1, Phase::Settled)], cfg(), t);
        t += COALESCE;
        assert_eq!(n.take_due(cfg(), t), None, "inside the rate limit");
        n.observe(&[row(1, Phase::Working)], cfg(), t);
        t += RATE_LIMIT;
        n.observe(&[row(1, Phase::Settled)], cfg(), t);
        assert!(n.take_due(cfg(), t + COALESCE).is_some());
    }

    #[test]
    fn attach_grace_baselines_existing_blocked_agents() {
        let t0 = Instant::now();
        let mut n = Notifier::new(t0);
        n.observe(&[row(1, Phase::Blocked)], cfg(), t0);
        assert_eq!(n.deadline(), None);
        // A new blocked pane after the grace does fire.
        let late = t0 + ATTACH_GRACE;
        n.observe(
            &[row(1, Phase::Blocked), row(2, Phase::Blocked)],
            cfg(),
            late,
        );
        let note = n.take_due(cfg(), late + COALESCE).expect("due");
        assert_eq!(note.title, "claude needs approval");
    }

    #[test]
    fn losing_window_focus_lets_the_focused_pane_notify() {
        let t0 = Instant::now();
        let mut n = Notifier::new(t0);
        let t1 = t0 + ATTACH_GRACE;
        let mut working = row(1, Phase::Working);
        working.viewing = true;
        n.observe(std::slice::from_ref(&working), cfg(), t1);
        n.set_client_focused(false);
        let mut done = row(1, Phase::Settled);
        done.viewing = true;
        n.observe(&[done], cfg(), t1);
        assert!(n.take_due(cfg(), t1 + COALESCE).is_some());
    }
}
