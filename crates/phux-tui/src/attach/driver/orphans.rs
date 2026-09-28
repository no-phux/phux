//! Best-effort kills for satellite panes this client spawned whose attach
//! was then refused (the satellite already runs them and nothing references
//! them). Replies are consumed here and logged at debug, never surfaced.
//!
//! A session switch sends no kill on the way out (the hub would hold the
//! re-attach behind it); those panes become *strays*, killed once their
//! satellite next answers. A late kill could hit a pane that is no longer a
//! stray, so each retry is one of two kinds ([`StrayKill`]):
//!
//! - **Conditional** (ADR-0109): the spawn was bound to the satellite's
//!   instance token and the hub supports `CONDITIONAL_KILL`, so the retry is
//!   a `KILL_RESOURCE_IF` the satellite refuses after a restart or any other
//!   use. That makes retrying safe even after unreachable signals, and lets
//!   the record wait longer ([`BOUND_STRAY_TTL`]).
//! - **Unconditional**: kept small and short-lived ([`STRAY_CAP`],
//!   [`STRAY_TTL`]) and forgotten on any sign the satellite was unreachable,
//!   since that cannot be told from a restart.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use phux_client::conditional_kill::BoundResource;
use phux_protocol::ResourceId;
use phux_protocol::ids::{SatelliteHost, ServerInstance};
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind, SpawnError};

use crate::attach::actions::SpawnedPane;

/// At most this many strays are remembered; recording another forgets the
/// oldest.
pub(super) const STRAY_CAP: usize = 32;

/// An unconditional stray whose satellite has not answered for this long is
/// forgotten; short, because the only unwanted-check covers this client's
/// windows alone.
pub(super) const STRAY_TTL: Duration = Duration::from_secs(60);

/// A conditional stray whose satellite has not answered for this long is
/// forgotten. Not a safety bound (the satellite judges the kill); it just
/// outlasts a link flap or short sleep plus the next retry trigger.
pub(super) const BOUND_STRAY_TTL: Duration = Duration::from_mins(10);

/// How a stray's one retry kills it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrayKill {
    /// `KILL_RESOURCE`: phux-c2td.23's retry, for a stray that could not be
    /// bound or a hub without `CONDITIONAL_KILL`.
    Unconditional,
    /// `KILL_RESOURCE_IF` under this instance token with
    /// `UNATTACHED_SINCE_SPAWN`.
    Conditional(ServerInstance),
}

impl StrayKill {
    /// The kill kind for a spawn bound to `instance`, on a hub with or
    /// without conditional kills.
    const fn for_spawn(instance: Option<ServerInstance>, conditional_kill: bool) -> Self {
        match instance {
            Some(instance) if conditional_kill => Self::Conditional(instance),
            _ => Self::Unconditional,
        }
    }

    /// How long a stray retried this way waits for its satellite.
    const fn ttl(self) -> Duration {
        match self {
            Self::Unconditional => STRAY_TTL,
            Self::Conditional(_) => BOUND_STRAY_TTL,
        }
    }

    /// Whether an unreachable signal leaves the stray waiting (a conditional
    /// kill is judged by the satellite, so it does).
    const fn outlives_unreachable(self) -> bool {
        matches!(self, Self::Conditional(_))
    }

    /// The command that kills `pane` this way.
    fn command(self, pane: ResourceId) -> Command {
        match self {
            Self::Unconditional => Command::KillResource {
                terminal_id: pane,
                operation_id: None,
            },
            Self::Conditional(instance) => BoundResource { id: pane, instance }.kill_command(),
        }
    }
}

/// A pane waiting for its satellite to answer before it is killed.
#[derive(Debug)]
pub(super) struct Stray {
    pane: ResourceId,
    kill: StrayKill,
    recorded_at: Instant,
}

impl Stray {
    /// The pane this stray's kill names.
    pub(super) const fn pane(&self) -> &ResourceId {
        &self.pane
    }
}

/// The orphan kills in flight, by request id, and the strays waiting for
/// their satellite to answer.
#[derive(Debug, Default)]
pub(super) struct OrphanKills {
    in_flight: HashMap<u32, ResourceId>,
    strays: VecDeque<Stray>,
    /// Spawns still unanswered when the loop left for a switch; a satellite
    /// pane among their replies becomes a stray.
    switch_spawns: HashSet<u32>,
    /// Whether the hub supports `CONDITIONAL_KILL` (set per loop entry).
    conditional_kill: bool,
}

impl OrphanKills {
    /// Retry bound strays through `KILL_RESOURCE_IF` exactly when
    /// `supported`, the hub's `CONDITIONAL_KILL` bit.
    pub(super) const fn set_conditional_kill(&mut self, supported: bool) {
        self.conditional_kill = supported;
    }

    /// One `KILL_RESOURCE` per orphaned pane, each under a fresh request id
    /// taken from `next_request_id` and tracked so its reply settles here.
    pub(super) fn kill_frames(
        &mut self,
        panes: Vec<ResourceId>,
        next_request_id: &mut u32,
    ) -> Vec<FrameKind> {
        panes
            .into_iter()
            .map(|pane| {
                let command = Command::KillResource {
                    terminal_id: pane.clone(),
                    operation_id: None,
                };
                self.track(pane, command, next_request_id)
            })
            .collect()
    }

    /// Each stray's single retry, of its kind. A failed kill is never
    /// recorded, so a refused conditional kill is never followed by an
    /// unconditional one.
    pub(super) fn stray_kill_frames(
        &mut self,
        strays: Vec<Stray>,
        next_request_id: &mut u32,
    ) -> Vec<FrameKind> {
        strays
            .into_iter()
            .map(|stray| {
                let command = stray.kill.command(stray.pane.clone());
                self.track(stray.pane, command, next_request_id)
            })
            .collect()
    }

    /// Frame `command`, the kill of `pane`, under the next request id, and
    /// remember it so the reply settles here.
    fn track(
        &mut self,
        pane: ResourceId,
        command: Command,
        next_request_id: &mut u32,
    ) -> FrameKind {
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        self.in_flight.insert(request_id, pane);
        FrameKind::Command {
            request_id,
            command,
        }
    }

    /// Forget unconditional strays on any satellite this frame shows
    /// unreachable, then consume a reply to one of these kills; any other
    /// frame is handed back.
    pub(super) fn observe(&mut self, frame: FrameKind) -> Option<FrameKind> {
        self.forget_unreachable(&frame);
        let Some(pane) = reply_request_id(&frame).and_then(|id| self.in_flight.remove(&id)) else {
            return Some(frame);
        };
        log_kill_reply(&pane, &frame);
        None
    }

    /// A `SATELLITE_UNREACHABLE` anywhere forgets the unconditional strays on
    /// the satellite it names (all of them when it names none): that
    /// satellite may be restarting and reuse a stray's id.
    fn forget_unreachable(&mut self, frame: &FrameKind) {
        let Some(message) = unreachable_message(frame) else {
            return;
        };
        match named_host(message) {
            Some(host) => {
                self.forget_unreachable_where(|pane| {
                    pane.host().is_some_and(|h| h.as_str() == host)
                });
            }
            None => self.forget_unreachable_where(|_| true),
        }
    }

    /// Forget the unconditional strays on `hosts`, which were just seen
    /// unreachable.
    pub(super) fn forget_hosts(&mut self, hosts: &[SatelliteHost]) {
        self.forget_unreachable_where(|pane| pane.host().is_some_and(|host| hosts.contains(host)));
    }

    /// Forget the strays on a satellite just seen unreachable (`doomed`),
    /// except the ones whose conditional kill that satellite will judge.
    fn forget_unreachable_where(&mut self, doomed: impl Fn(&ResourceId) -> bool) {
        self.strays.retain(|stray| {
            let forget = !stray.kill.outlives_unreachable() && doomed(&stray.pane);
            if forget {
                tracing::debug!(pane = ?stray.pane, "forgetting a stray satellite pane: its satellite was unreachable");
            }
            !forget
        });
    }

    /// Remember one stray, once; past [`STRAY_CAP`] the oldest is forgotten.
    fn record(&mut self, pane: ResourceId, kill: StrayKill, now: Instant) {
        if self.strays.iter().any(|stray| stray.pane == pane) {
            return;
        }
        if self.strays.len() >= STRAY_CAP
            && let Some(oldest) = self.strays.pop_front()
        {
            tracing::debug!(pane = ?oldest.pane, "forgetting the oldest stray satellite pane");
        }
        tracing::debug!(
            ?pane,
            ?kill,
            "remembering a stray satellite pane to kill later"
        );
        self.strays.push_back(Stray {
            pane,
            kill,
            recorded_at: now,
        });
    }

    /// Remember panes an unreachable satellite left running, for a
    /// conditional retry. Without `CONDITIONAL_KILL` this records nothing.
    pub(super) fn record_unreachable(&mut self, stranded: Vec<BoundResource>, now: Instant) {
        if !self.conditional_kill {
            return;
        }
        for bound in stranded {
            self.record(bound.id, StrayKill::Conditional(bound.instance), now);
        }
    }

    /// Take the strays on `hosts` recorded no later than `answered_at`,
    /// dropping expired ones first.
    pub(super) fn take_answered(
        &mut self,
        hosts: &[SatelliteHost],
        answered_at: Instant,
        now: Instant,
    ) -> Vec<Stray> {
        self.forget_expired(now);
        let (due, waiting) = std::mem::take(&mut self.strays)
            .into_iter()
            .partition(|stray| answered_by(stray, hosts, answered_at));
        self.strays = waiting;
        due.into()
    }

    fn forget_expired(&mut self, now: Instant) {
        self.strays.retain(|stray| {
            let fresh = now.saturating_duration_since(stray.recorded_at) <= stray.kill.ttl();
            if !fresh {
                tracing::debug!(pane = ?stray.pane, "forgetting a stray satellite pane: its satellite did not answer in time");
            }
            fresh
        });
    }

    /// Forget strays whose ids a satellite has minted again (ids only grow
    /// within one run, so a lower-or-equal fresh id means it restarted).
    pub(super) fn forget_reissued(&mut self, fresh: &[ResourceId]) {
        self.strays
            .retain(|stray| !fresh.iter().any(|pane| reissued_by(&stray.pane, pane)));
    }

    /// Leave for a session switch: remember parked spawned panes and the
    /// spawns still to answer.
    pub(super) fn park_for_switch(
        &mut self,
        spawned: Vec<SpawnedPane>,
        unanswered_spawns: HashSet<u32>,
        now: Instant,
    ) {
        for pane in spawned {
            self.record_spawned(pane, now);
        }
        self.switch_spawns = unanswered_spawns;
    }

    /// Remember a pane a session switch stranded, retried conditionally when
    /// its spawn was bound and the hub evaluates conditional kills.
    fn record_spawned(&mut self, pane: SpawnedPane, now: Instant) {
        let kill = StrayKill::for_spawn(pane.instance, self.conditional_kill);
        self.record(pane.id, kill, now);
    }

    /// A frame drained during the switch: observed as usual, and a satellite
    /// pane answering a parked spawn is remembered.
    pub(super) fn observe_switch_drain(&mut self, frame: FrameKind, now: Instant) {
        let Some(frame) = self.observe(frame) else {
            return;
        };
        if let Some(pane) = self.switch_spawn_reply(&frame) {
            self.record_spawned(pane, now);
        }
    }

    fn switch_spawn_reply(&mut self, frame: &FrameKind) -> Option<SpawnedPane> {
        let FrameKind::ResourceSpawned { request_id, result } = frame else {
            return None;
        };
        let id = result.spawned_id()?;
        (self.switch_spawns.remove(request_id) && !id.is_local()).then(|| SpawnedPane {
            id: id.clone(),
            instance: result.instance(),
        })
    }

    /// The switch reached `DETACHED`: clear the old request-id space (the
    /// next loop restarts at 1). The strays stay.
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

/// The message of a frame saying a satellite is unreachable.
const fn unreachable_message(frame: &FrameKind) -> Option<&String> {
    use phux_protocol::wire::frame::SpawnResult;
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

/// The satellite an unreachable message names (`satellite {host} ...`), or
/// `None`, which the caller treats as every satellite.
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

/// Log an orphan kill's outcome at debug; failures and conditional refusals
/// are expected, and the pane is not retried.
fn log_kill_reply(pane: &ResourceId, frame: &FrameKind) {
    match frame {
        FrameKind::CommandResult {
            result: CommandResult::Ok,
            ..
        } => tracing::debug!(?pane, "killed the orphaned satellite pane"),
        FrameKind::CommandResult {
            result: CommandResult::Error { code, message },
            ..
        }
        | FrameKind::Error { code, message, .. } => tracing::debug!(
            ?pane,
            ?code,
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
#[path = "orphans_tests.rs"]
mod tests;
