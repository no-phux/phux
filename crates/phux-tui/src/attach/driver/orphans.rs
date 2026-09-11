//! phux-c2td.20: best-effort kills for satellite panes this client spawned
//! whose attach was then refused.
//!
//! A window or split spawned on a satellite (`new-window { host }`,
//! `split-pane` on a satellite pane) opens only once its pane attaches. When
//! that attach is refused the TUI opens nothing, but the satellite already
//! runs the pane, and nothing references it. The driver kills it through the
//! hub with the same host-qualified `KILL_RESOURCE` `phux kill host/@id`
//! sends. The satellite is often why the attach failed, so the kill can fail
//! too: its reply is consumed here and logged at debug, never surfaced.
//!
//! phux-c2td.23: a session switch drops the windows and splits still
//! opening, and sends no kill on the way out: the hub waits on each relayed
//! command before it reads this client's next frame, so a kill would hold
//! up the re-attach. Their panes are *strays*, remembered here and killed
//! once their satellite next answers ([`OrphanKills::take_answered`]),
//! normally a moment after the switch. The record lives in the driver's
//! outer loop, so it survives the switch.
//!
//! A late kill can land on a pane that is no longer a stray: this client
//! only knows its own windows, and a satellite that restarted mints its ids
//! from 1 again. So the record stays small and short-lived ([`STRAY_CAP`],
//! [`STRAY_TTL`]), and any sign that a stray's satellite was unreachable
//! forgets it ([`OrphanKills::observe`], [`OrphanKills::forget_hosts`]):
//! an unreachable satellite cannot be told from one that is restarting.
//! A refusal that says the satellite is unreachable records nothing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use phux_protocol::ResourceId;
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnError, SpawnResult,
};

/// At most this many strays are remembered; recording another forgets the
/// oldest.
pub(super) const STRAY_CAP: usize = 32;

/// A stray whose satellite has not answered for this long is forgotten. The
/// retry normally fires on the first host inventory after the switch, well
/// inside this; it is kept short because the only check that the pane is
/// still unwanted covers this client's windows alone.
pub(super) const STRAY_TTL: Duration = Duration::from_secs(60);

/// A pane waiting for its satellite to answer before it is killed.
#[derive(Debug)]
struct Stray {
    pane: ResourceId,
    recorded_at: Instant,
}

/// The orphan kills in flight, by request id, and the strays waiting for
/// their satellite to answer.
#[derive(Debug, Default)]
pub(super) struct OrphanKills {
    in_flight: HashMap<u32, ResourceId>,
    strays: VecDeque<Stray>,
    /// Request ids of spawns still unanswered when the loop left for a
    /// session switch. Their replies arrive while the switch drains the
    /// connection; a satellite pane among them is recorded as a stray.
    switch_spawns: HashSet<u32>,
}

impl OrphanKills {
    /// One `KILL_RESOURCE` per orphaned pane, each under a fresh request id
    /// taken from `next_request_id` and tracked so its reply settles here.
    /// A stray's kill is its one attempt: it is out of the record by then,
    /// and a failed kill is never recorded.
    pub(super) fn kill_frames(
        &mut self,
        panes: Vec<ResourceId>,
        next_request_id: &mut u32,
    ) -> Vec<FrameKind> {
        panes
            .into_iter()
            .map(|pane| {
                let request_id = *next_request_id;
                *next_request_id = next_request_id.wrapping_add(1);
                self.in_flight.insert(request_id, pane.clone());
                FrameKind::Command {
                    request_id,
                    command: Command::KillResource { terminal_id: pane },
                }
            })
            .collect()
    }

    /// Look at one inbound frame: forget the strays on any satellite it
    /// shows unreachable ([`Self::forget_unreachable`]), then consume the
    /// reply to one of these kills, logging how it went. Any other frame is
    /// handed back.
    pub(super) fn observe(&mut self, frame: FrameKind) -> Option<FrameKind> {
        self.forget_unreachable(&frame);
        let Some(pane) = reply_request_id(&frame).and_then(|id| self.in_flight.remove(&id)) else {
            return Some(frame);
        };
        log_kill_reply(&pane, &frame);
        None
    }

    /// A `SATELLITE_UNREACHABLE` anywhere (a notice, a refused command or
    /// spawn, a failed kill) forgets the strays on the satellite its message
    /// names, and every stray when it names none: that satellite may be
    /// restarting, and its next panes could reuse a stray's id.
    fn forget_unreachable(&mut self, frame: &FrameKind) {
        let Some(message) = unreachable_message(frame) else {
            return;
        };
        match named_host(message) {
            Some(host) => self.forget_where(|pane| pane.host().is_some_and(|h| h.as_str() == host)),
            None => self.forget_where(|_| true),
        }
    }

    /// Forget the strays on `hosts`, which were just seen unreachable.
    pub(super) fn forget_hosts(&mut self, hosts: &[SatelliteHost]) {
        self.forget_where(|pane| pane.host().is_some_and(|host| hosts.contains(host)));
    }

    fn forget_where(&mut self, doomed: impl Fn(&ResourceId) -> bool) {
        self.strays.retain(|stray| {
            let forget = doomed(&stray.pane);
            if forget {
                tracing::debug!(pane = ?stray.pane, "forgetting a stray satellite pane: its satellite was unreachable");
            }
            !forget
        });
    }

    /// Remember one stray, once; past [`STRAY_CAP`] the oldest is forgotten.
    fn record(&mut self, pane: ResourceId, now: Instant) {
        if self.strays.iter().any(|stray| stray.pane == pane) {
            return;
        }
        if self.strays.len() >= STRAY_CAP
            && let Some(oldest) = self.strays.pop_front()
        {
            tracing::debug!(pane = ?oldest.pane, "forgetting the oldest stray satellite pane");
        }
        tracing::debug!(?pane, "remembering a stray satellite pane to kill later");
        self.strays.push_back(Stray {
            pane,
            recorded_at: now,
        });
    }

    /// Take the strays on `hosts` recorded no later than `answered_at`, the
    /// moment those satellites were known to answer. Strays older than
    /// [`STRAY_TTL`] at `now` are forgotten first.
    pub(super) fn take_answered(
        &mut self,
        hosts: &[SatelliteHost],
        answered_at: Instant,
        now: Instant,
    ) -> Vec<ResourceId> {
        self.forget_expired(now);
        let (due, waiting) = std::mem::take(&mut self.strays)
            .into_iter()
            .partition(|stray| answered_by(stray, hosts, answered_at));
        self.strays = waiting;
        due.into_iter().map(|stray: Stray| stray.pane).collect()
    }

    fn forget_expired(&mut self, now: Instant) {
        self.strays.retain(|stray| {
            let fresh = now.saturating_duration_since(stray.recorded_at) <= STRAY_TTL;
            if !fresh {
                tracing::debug!(pane = ?stray.pane, "forgetting a stray satellite pane: its satellite did not answer in time");
            }
            fresh
        });
    }

    /// Forget strays whose ids a satellite has since minted again: a fresh
    /// pane on the same host with an id no greater than a stray's means that
    /// satellite restarted (its ids only ever grow within one run). A guard
    /// on top of [`Self::forget_unreachable`], which catches most restarts
    /// first; it cannot see one whose new ids have already passed the stray's.
    pub(super) fn forget_reissued(&mut self, fresh: &[ResourceId]) {
        self.strays
            .retain(|stray| !fresh.iter().any(|pane| reissued_by(&stray.pane, pane)));
    }

    /// Leave for a session switch: remember the panes the loop's parked
    /// windows and splits spawned, and the spawns whose replies are still to
    /// come ([`Self::observe_switch_drain`]).
    pub(super) fn park_for_switch(
        &mut self,
        spawned: Vec<ResourceId>,
        unanswered_spawns: HashSet<u32>,
        now: Instant,
    ) {
        for pane in spawned {
            self.record(pane, now);
        }
        self.switch_spawns = unanswered_spawns;
    }

    /// One frame the switch drained before `DETACHED`: observed as usual
    /// ([`Self::observe`]), and a satellite pane answering one of the parked
    /// spawns is remembered.
    pub(super) fn observe_switch_drain(&mut self, frame: FrameKind, now: Instant) {
        let Some(frame) = self.observe(frame) else {
            return;
        };
        if let Some(pane) = self.switch_spawn_reply(&frame) {
            self.record(pane, now);
        }
    }

    fn switch_spawn_reply(&mut self, frame: &FrameKind) -> Option<ResourceId> {
        let FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(pane),
        } = frame
        else {
            return None;
        };
        (self.switch_spawns.remove(request_id) && !pane.is_local()).then(|| pane.clone())
    }

    /// The switch reached `DETACHED`, so every reply to the old loop's
    /// requests has arrived; the next loop counts request ids from 1 again,
    /// so nothing of the old id space may linger. The strays stay.
    pub(super) fn finish_switch_drain(&mut self) {
        self.in_flight.clear();
        self.switch_spawns.clear();
    }

    /// Test seam: make every stray `by` older, as if it were recorded then.
    #[cfg(test)]
    pub(super) fn age_strays(&mut self, by: Duration) {
        for stray in &mut self.strays {
            stray.recorded_at = stray
                .recorded_at
                .checked_sub(by)
                .unwrap_or(stray.recorded_at);
        }
    }
}

/// Whether `stray` is on one of `hosts` and was recorded no later than
/// `answered_at`, so the answer is news about it.
fn answered_by(stray: &Stray, hosts: &[SatelliteHost], answered_at: Instant) -> bool {
    stray.recorded_at <= answered_at && stray.pane.host().is_some_and(|host| hosts.contains(host))
}

/// Whether `fresh`, a pane its satellite just minted, shows that `stray`'s
/// id belongs to an earlier run of that satellite.
fn reissued_by(stray: &ResourceId, fresh: &ResourceId) -> bool {
    match (stray, fresh) {
        (
            ResourceId::Satellite { host, id },
            ResourceId::Satellite {
                host: fresh_host,
                id: fresh_id,
            },
        ) => host == fresh_host && id >= fresh_id,
        _ => false,
    }
}

/// The message of a frame that says a satellite is unreachable: an `ERROR`
/// or `COMMAND_RESULT` error with `SATELLITE_UNREACHABLE`, or a spawn
/// refused with `SatelliteUnreachable`.
const fn unreachable_message(frame: &FrameKind) -> Option<&String> {
    match frame {
        FrameKind::Error {
            code: ErrorCode::SatelliteUnreachable,
            message,
            ..
        }
        | FrameKind::CommandResult {
            result:
                CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message,
                },
            ..
        }
        | FrameKind::ResourceSpawned {
            result: SpawnResult::Err(SpawnError::SatelliteUnreachable(message)),
            ..
        } => Some(message),
        _ => None,
    }
}

/// The satellite an unreachable message names. The hub words every one as
/// `satellite {host} ...` (`is unreachable: ...`, `link is down`, `did not
/// answer ...`); `None` for any other shape, which the caller treats as
/// naming every satellite.
fn named_host(message: &str) -> Option<&str> {
    let (host, _) = message.strip_prefix("satellite ")?.split_once(' ')?;
    Some(host)
}

/// The request id a reply frame answers, if it is a reply.
const fn reply_request_id(frame: &FrameKind) -> Option<u32> {
    match frame {
        FrameKind::CommandResult { request_id, .. } => Some(*request_id),
        FrameKind::Error { request_id, .. } => *request_id,
        _ => None,
    }
}

/// Log an orphan kill's outcome. A failure is expected when the satellite
/// is unreachable, so it stays at debug, and the pane is not tried again.
fn log_kill_reply(pane: &ResourceId, frame: &FrameKind) {
    match frame {
        FrameKind::CommandResult {
            result: CommandResult::Ok,
            ..
        } => tracing::debug!(?pane, "killed the orphaned satellite pane"),
        FrameKind::CommandResult {
            result: CommandResult::Error { message, .. },
            ..
        }
        | FrameKind::Error { message, .. } => tracing::debug!(
            ?pane,
            %message,
            "could not kill the orphaned satellite pane; dropping it",
        ),
        _ => tracing::debug!(
            ?pane,
            "unexpected reply to an orphaned satellite pane's kill"
        ),
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::frame::{ErrorCode, SpawnError};

    use super::*;

    fn edge(id: u32) -> ResourceId {
        ResourceId::satellite(SatelliteHost::new("edge"), id)
    }

    fn other(id: u32) -> ResourceId {
        ResourceId::satellite(SatelliteHost::new("other"), id)
    }

    fn hosts(names: &[&str]) -> Vec<SatelliteHost> {
        names.iter().map(|name| SatelliteHost::new(*name)).collect()
    }

    fn unreachable_reply(request_id: u32, host: &str) -> FrameKind {
        FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::SatelliteUnreachable,
            message: format!("satellite {host} link is down"),
        }
    }

    /// Strays recorded as a switch would, with no spawns still unanswered.
    fn with_strays(panes: Vec<ResourceId>, now: Instant) -> OrphanKills {
        let mut kills = OrphanKills::default();
        kills.park_for_switch(panes, HashSet::new(), now);
        kills
    }

    /// Every stray still waiting, whatever its host.
    fn waiting(kills: &mut OrphanKills, now: Instant) -> Vec<ResourceId> {
        let mut due = kills.take_answered(&hosts(&["edge", "other"]), now, now);
        due.sort();
        due
    }

    #[test]
    fn one_kill_per_orphan_under_fresh_request_ids() {
        let mut kills = OrphanKills::default();
        let mut next = 40;
        let frames = kills.kill_frames(vec![edge(9), edge(10)], &mut next);
        assert_eq!(
            frames,
            vec![
                FrameKind::Command {
                    request_id: 40,
                    command: Command::KillResource {
                        terminal_id: edge(9)
                    },
                },
                FrameKind::Command {
                    request_id: 41,
                    command: Command::KillResource {
                        terminal_id: edge(10)
                    },
                },
            ]
        );
        assert_eq!(next, 42);
    }

    #[test]
    fn a_kill_reply_is_consumed_once_whatever_it_says() {
        for reply in [
            FrameKind::CommandResult {
                request_id: 7,
                result: CommandResult::Ok,
            },
            FrameKind::CommandResult {
                request_id: 7,
                result: CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message: "satellite edge link is down".to_owned(),
                },
            },
            unreachable_reply(7, "edge"),
        ] {
            let mut kills = OrphanKills::default();
            let mut next = 7;
            kills.kill_frames(vec![edge(9)], &mut next);
            assert_eq!(kills.observe(reply.clone()), None, "{reply:?}");
            assert_eq!(
                kills.observe(reply.clone()),
                Some(reply),
                "a second reply is not ours"
            );
        }
    }

    #[test]
    fn other_frames_pass_through() {
        let mut kills = OrphanKills::default();
        let mut next = 7;
        kills.kill_frames(vec![edge(9)], &mut next);
        let unrelated = FrameKind::CommandResult {
            request_id: 8,
            result: CommandResult::Ok,
        };
        assert_eq!(kills.observe(unrelated.clone()), Some(unrelated));
        let uncorrelated = FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite edge is unreachable".to_owned(),
        };
        assert_eq!(kills.observe(uncorrelated.clone()), Some(uncorrelated));
    }

    /// phux-c2td.23: a stray is handed back once its satellite answers, and
    /// only its satellite: another one answering says nothing about it.
    #[test]
    fn a_stray_is_due_once_its_host_answers() {
        let recorded = Instant::now();
        let mut kills = with_strays(vec![edge(9)], recorded);
        assert_eq!(
            kills.take_answered(&hosts(&["other"]), recorded, recorded),
            Vec::new()
        );
        assert_eq!(
            kills.take_answered(&hosts(&["edge"]), recorded, recorded),
            vec![edge(9)]
        );
        assert_eq!(
            kills.take_answered(&hosts(&["edge"]), recorded, recorded),
            Vec::new(),
            "handed back once"
        );
    }

    /// An answer from before the stray was recorded is not news about it.
    #[test]
    fn an_answer_older_than_the_stray_does_not_retry_it() {
        let asked = Instant::now();
        let recorded = asked + Duration::from_secs(1);
        let mut kills = with_strays(vec![edge(9)], recorded);
        assert_eq!(
            kills.take_answered(&hosts(&["edge"]), asked, recorded),
            Vec::new()
        );
        assert_eq!(
            kills.take_answered(&hosts(&["edge"]), recorded, recorded),
            vec![edge(9)]
        );
    }

    /// The record keeps at most [`STRAY_CAP`] panes, dropping the oldest,
    /// and never holds one pane twice.
    #[test]
    fn the_record_is_capped_oldest_first_and_deduplicated() {
        let now = Instant::now();
        let cap = u32::try_from(STRAY_CAP).unwrap();
        let mut kills = with_strays((1..=cap + 2).map(edge).collect(), now);
        kills.park_for_switch(vec![edge(cap + 2)], HashSet::new(), now);
        let due = kills.take_answered(&hosts(&["edge"]), now, now);
        assert_eq!(due, (3..=cap + 2).map(edge).collect::<Vec<_>>());
    }

    /// A stray is still killed when its satellite answers within
    /// [`STRAY_TTL`], and forgotten when the answer comes any later.
    #[test]
    fn a_stray_is_due_within_its_ttl_and_forgotten_after() {
        let recorded = Instant::now();
        let within = recorded + STRAY_TTL;
        let mut kills = with_strays(vec![edge(9)], recorded);
        assert_eq!(
            kills.take_answered(&hosts(&["edge"]), within, within),
            vec![edge(9)]
        );

        let past = within + Duration::from_secs(1);
        let mut kills = with_strays(vec![edge(9)], recorded);
        assert_eq!(
            kills.take_answered(&hosts(&["edge"]), past, past),
            Vec::new()
        );
    }

    /// A kill is one attempt: a failed one is never recorded, so a stray
    /// whose kill fails is dropped after that one retry.
    #[test]
    fn a_failed_kill_is_not_remembered() {
        let now = Instant::now();
        let mut kills = with_strays(vec![edge(9)], now);
        let due = kills.take_answered(&hosts(&["edge"]), now, now);
        let mut next = 1;
        kills.kill_frames(due, &mut next);
        kills.kill_frames(vec![other(4)], &mut next);
        let busy = FrameKind::Error {
            request_id: Some(1),
            code: ErrorCode::ResourceExhausted,
            message: "satellite edge link is saturated; retry".to_owned(),
        };
        assert_eq!(kills.observe(busy), None);
        assert_eq!(kills.observe(unreachable_reply(2, "other")), None);
        assert_eq!(waiting(&mut kills, now), Vec::new());
    }

    /// Any frame saying a satellite is unreachable forgets that satellite's
    /// strays and keeps the rest: an uncorrelated notice, a refused command,
    /// a refused spawn. A message naming no satellite forgets them all; a
    /// failure for another reason forgets nothing.
    #[test]
    fn an_unreachable_satellite_forgets_its_strays() {
        let now = Instant::now();
        let edge_down = [
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::SatelliteUnreachable,
                message: "satellite edge is unreachable: link is down".to_owned(),
            },
            FrameKind::CommandResult {
                request_id: 3,
                result: CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message: "satellite edge did not answer within 30s".to_owned(),
                },
            },
            FrameKind::ResourceSpawned {
                request_id: 3,
                result: SpawnResult::Err(SpawnError::SatelliteUnreachable(
                    "satellite edge link is down".to_owned(),
                )),
            },
        ];
        for frame in edge_down {
            let mut kills = with_strays(vec![edge(9), other(4)], now);
            kills.observe(frame.clone());
            assert_eq!(waiting(&mut kills, now), vec![other(4)], "{frame:?}");
        }

        let mut kills = with_strays(vec![edge(9), other(4)], now);
        kills.observe(FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "unreachable".to_owned(),
        });
        assert_eq!(waiting(&mut kills, now), Vec::new(), "no host named");

        let mut kills = with_strays(vec![edge(9)], now);
        kills.observe(FrameKind::CommandResult {
            request_id: 3,
            result: CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "satellite edge has no such terminal".to_owned(),
            },
        });
        assert_eq!(waiting(&mut kills, now), vec![edge(9)], "not unreachable");
    }

    /// A host inventory's unreachable rows forget those satellites' strays.
    #[test]
    fn unreachable_hosts_forget_their_strays() {
        let now = Instant::now();
        let mut kills = with_strays(vec![edge(9), other(4)], now);
        kills.forget_hosts(&hosts(&["edge"]));
        assert_eq!(waiting(&mut kills, now), vec![other(4)]);
    }

    /// A pane freshly minted on the same host with an id no greater than a
    /// stray's means that satellite restarted: the stray is forgotten, and a
    /// stray on another host, or with a lower id, is kept.
    #[test]
    fn a_reissued_id_forgets_the_stray() {
        let now = Instant::now();
        let mut kills = with_strays(vec![edge(3), edge(9), other(9)], now);
        kills.forget_reissued(&[edge(5)]);
        assert_eq!(waiting(&mut kills, now), vec![edge(3), other(9)]);
    }

    /// A session switch remembers the parked panes and the satellite panes
    /// its unanswered spawns turn out to be; the drain settles kill replies,
    /// forgets a satellite it shows unreachable, and clears the old
    /// request-id space.
    #[test]
    fn a_switch_remembers_parked_and_drained_spawns() {
        let mut kills = OrphanKills::default();
        let mut next = 50;
        kills.kill_frames(vec![other(1)], &mut next);
        let now = Instant::now();
        kills.park_for_switch(vec![edge(9), other(2)], HashSet::from([20, 21, 22]), now);
        for frame in [
            FrameKind::ResourceSpawned {
                request_id: 20,
                result: SpawnResult::Ok(edge(10)),
            },
            FrameKind::ResourceSpawned {
                request_id: 21,
                result: SpawnResult::Ok(ResourceId::local(4)),
            },
            FrameKind::ResourceSpawned {
                request_id: 22,
                result: SpawnResult::Err(SpawnError::SpawnFailed("no shell".to_owned())),
            },
            FrameKind::ResourceSpawned {
                request_id: 23,
                result: SpawnResult::Ok(edge(11)),
            },
            unreachable_reply(50, "other"),
        ] {
            kills.observe_switch_drain(frame, now);
        }
        kills.finish_switch_drain();
        assert!(kills.in_flight.is_empty() && kills.switch_spawns.is_empty());
        assert_eq!(waiting(&mut kills, now), vec![edge(9), edge(10)]);
    }
}
