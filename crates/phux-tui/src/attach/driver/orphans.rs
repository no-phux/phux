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
//! from 1 again. Each stray's retry is therefore one of two kinds
//! ([`StrayKill`]):
//!
//! - **Conditional** (phux-c2td.25, ADR-0109): the spawn was bound to the
//!   satellite's instance token and the hub advertises `CONDITIONAL_KILL`.
//!   The retry is a `KILL_RESOURCE_IF` the satellite refuses if the token is
//!   stale (it restarted) or anyone else attached or used the pane. That
//!   makes a stray an unreachable satellite strands safe to retry too
//!   ([`OrphanKills::record_unreachable`]), and lets the record wait longer
//!   ([`BOUND_STRAY_TTL`]) and outlive signs that the satellite was
//!   unreachable. A refused conditional kill is never followed by an
//!   unconditional one.
//! - **Unconditional**: a session-switch stray whose spawn was not bound, or
//!   any stray on a hub without the bit. The record stays small and
//!   short-lived ([`STRAY_CAP`], [`STRAY_TTL`]), and any sign that the
//!   satellite was unreachable forgets it ([`OrphanKills::observe`],
//!   [`OrphanKills::forget_hosts`]): an unreachable satellite cannot be told
//!   from one that is restarting. Without the bit, a refusal that says the
//!   satellite is unreachable records nothing.

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
/// forgotten. The retry normally fires on the first host inventory after the
/// switch, well inside this; it is kept short because the only check that
/// the pane is still unwanted covers this client's windows alone.
pub(super) const STRAY_TTL: Duration = Duration::from_secs(60);

/// A conditional stray whose satellite has not answered for this long is
/// forgotten. This is not a safety bound: the satellite checks the kill
/// whenever it arrives, refusing it after a restart or any other use of the
/// pane, so waiting longer only risks leaking a pane, never killing someone
/// else's. It bounds how long a stray waits for a retry trigger, which for a
/// satellite that was unreachable is a link recovery followed by the next
/// host inventory (a session's first paint or a session-picker open) or a
/// spawn there. Ten minutes outlasts a link flap, a redial backoff or a
/// short laptop sleep plus that next trigger; past it the user has likely
/// moved on, `phux ls` still lists the pane, and the hub's bounded spawn
/// ledger grows ever more likely to have forgotten it (so refusing it).
pub(super) const BOUND_STRAY_TTL: Duration = Duration::from_mins(10);

/// How a stray's one retry kills it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrayKill {
    /// `KILL_RESOURCE`: phux-c2td.23's retry, for a stray that could not be
    /// bound or a hub without `CONDITIONAL_KILL`.
    Unconditional,
    /// `KILL_RESOURCE_IF` under this instance token with
    /// `UNATTACHED_SINCE_SPAWN` (phux-c2td.25).
    Conditional(ServerInstance),
}

impl StrayKill {
    /// The kill for a pane this client spawned under `instance`, if its
    /// spawn was bound, on a hub that does or does not evaluate conditional
    /// kills.
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

    /// Whether a sign that the stray's satellite was unreachable leaves it
    /// waiting. A conditional kill is: the satellite evaluates it, and a
    /// restart the silence may have hidden changes its token.
    const fn outlives_unreachable(self) -> bool {
        matches!(self, Self::Conditional(_))
    }

    /// The command that kills `pane` this way.
    fn command(self, pane: ResourceId) -> Command {
        match self {
            Self::Unconditional => Command::KillResource { terminal_id: pane },
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
    /// Request ids of spawns still unanswered when the loop left for a
    /// session switch. Their replies arrive while the switch drains the
    /// connection; a satellite pane among them is recorded as a stray.
    switch_spawns: HashSet<u32>,
    /// Whether the hub advertises `CONDITIONAL_KILL`, so a bound stray is
    /// retried conditionally. Set by each loop entry from the connection's
    /// negotiated features.
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
                };
                self.track(pane, command, next_request_id)
            })
            .collect()
    }

    /// The retry of each stray, as its kind says ([`StrayKill`]), tracked
    /// like any orphan kill. It is the stray's one attempt: the stray is out
    /// of the record by then, and a failed kill is never recorded, so a
    /// refused conditional kill is never followed by an unconditional one.
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

    /// Look at one inbound frame: forget the unconditional strays on any
    /// satellite it shows unreachable ([`Self::forget_unreachable`]), then
    /// consume the reply to one of these kills, logging how it went. Any
    /// other frame is handed back.
    pub(super) fn observe(&mut self, frame: FrameKind) -> Option<FrameKind> {
        self.forget_unreachable(&frame);
        let Some(pane) = reply_request_id(&frame).and_then(|id| self.in_flight.remove(&id)) else {
            return Some(frame);
        };
        log_kill_reply(&pane, &frame);
        None
    }

    /// A `SATELLITE_UNREACHABLE` anywhere (a notice, a refused command or
    /// spawn, a failed kill) forgets the unconditional strays on the
    /// satellite its message names, and on every satellite when it names
    /// none: that satellite may be restarting, and its next panes could
    /// reuse a stray's id.
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

    /// phux-c2td.25: remember panes a refusal saying their satellite is
    /// unreachable left running, each bound to that satellite's instance
    /// token, to retry through the conditional kill once it answers again.
    /// Without `CONDITIONAL_KILL` this records nothing, as before: an
    /// unconditional retry could land on someone else's pane.
    pub(super) fn record_unreachable(&mut self, stranded: Vec<BoundResource>, now: Instant) {
        if !self.conditional_kill {
            return;
        }
        for bound in stranded {
            self.record(bound.id, StrayKill::Conditional(bound.instance), now);
        }
    }

    /// Take the strays on `hosts` recorded no later than `answered_at`, the
    /// moment those satellites were known to answer. Strays past their
    /// expiry ([`STRAY_TTL`], [`BOUND_STRAY_TTL`]) at `now` are forgotten
    /// first.
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

    /// Forget strays whose ids a satellite has since minted again: a fresh
    /// pane on the same host with an id no greater than a stray's means that
    /// satellite restarted (its ids only ever grow within one run). A guard
    /// on top of [`Self::forget_unreachable`], which catches most restarts
    /// first; it cannot see one whose new ids have already passed the stray's.
    /// A conditional stray is forgotten too: its stale token would be refused.
    pub(super) fn forget_reissued(&mut self, fresh: &[ResourceId]) {
        self.strays
            .retain(|stray| !fresh.iter().any(|pane| reissued_by(&stray.pane, pane)));
    }

    /// Leave for a session switch: remember the panes the loop's parked
    /// windows and splits spawned, and the spawns whose replies are still to
    /// come ([`Self::observe_switch_drain`]).
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

    /// One frame the switch drained before `DETACHED`: observed as usual
    /// ([`Self::observe`]), and a satellite pane answering one of the parked
    /// spawns is remembered.
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
/// is unreachable, and a conditional kill is refused (`PRECONDITION_FAILED`,
/// `TERMINAL_NOT_FOUND`) whenever the pane is no longer this client's to
/// kill, so either stays at debug, and the pane is not tried again.
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
