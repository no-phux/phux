//! Hub-to-satellite frame relay (phux-v45.4, ADR-0007 §4).
//!
//! The routing layer that replaces the blanket `UnsupportedSatelliteRoute`
//! rejections on a `phux server --hub`: a frame targeting
//! `TerminalId::Satellite { host, id }` is resolved to the live outbound
//! link the dialer (phux-v45.3) maintains for `host`, its terminal id is
//! rewritten to `Local { id }`, and the frame is forwarded **verbatim** —
//! the hub never re-encodes VT bytes (ADR-0007: opaque relay). Responses
//! and subscribed streams coming back from the satellite are re-tagged
//! `Local { id }` -> `Satellite { host, id }` before they reach the
//! consumer, so the consumer only ever sees the hub-scoped address it
//! asked for. Satellites stay unaware of each other: a frame arriving
//! from a satellite already tagged `Satellite` is dropped, never chained.
//!
//! One `RelaySession` lives inside each link supervisor
//! (`super::link::run_link`) while its connection is up. It owns the
//! per-link `request_id` remap (the hub allocates its own id space toward
//! the satellite; the consumer's `request_id` never crosses the link) and
//! the proxy-subscription registry (which hub consumers observe which
//! satellite terminals). Consumers talk to it through a `RelayHandle` —
//! a bounded mailbox published in `HubRelays` on shared state.
//!
//! **Fail fast, never hang.** No live connection means a typed
//! `ErrorCode::SatelliteUnreachable` reply, immediately: the link
//! supervisor drains the relay mailbox during dial, backoff, and
//! fail-closed refusal phases, failing every queued request. A satellite
//! disconnect fails all in-flight commands the same way and pushes one
//! typed `ERROR { SatelliteUnreachable }` frame to every proxy-subscribed
//! consumer before the registry is cleared — teardown is observable, not
//! silence. A *silently* dead satellite — one whose link still looks
//! `Connected` because the network partitioned without FIN/RST, or one
//! that reads frames but never answers — is bounded twice over: every
//! relayed command carries a hub-side deadline (`RELAY_COMMAND_TIMEOUT`)
//! resolving to the same typed error, and the link supervisor enforces a
//! transport keepalive / idle contract (`super::link`) so the partition
//! itself is detected and torn down. Abandoned entries in the
//! pending-command map are pruned on the supervisor's keepalive tick
//! (`RelaySession::prune_abandoned`), so a satellite that swallows
//! frames cannot grow hub state without bound.
//!
//! **Backpressure.** The relay mailbox is bounded (`RELAY_MAILBOX`) and
//! every producer uses `try_send`: a saturated link fails commands with
//! `ResourceExhausted` and drops fire-and-forget frames with a warn,
//! mirroring the pane-input mailbox semantics. Return-leg fan-out to
//! consumers uses `try_send` into each consumer's bounded outbound
//! mailbox (the same discipline as `crate::runtime::client::broadcast_event`),
//! so one slow consumer never stalls the link or the hub's other work.
//! The one exception is the attach ordering anchor (phux-v45.12, L1 §9.1):
//! a return-leg `TERMINAL_SNAPSHOT` a briefly-full consumer refuses is
//! *retained* per subscriber and that consumer's later deltas are
//! suppressed until it lands, so a `TERMINAL_OUTPUT` can never overtake the
//! snapshot across the two-hop attach — the non-blocking mirror of the
//! local attach's snapshot gate (`RelaySession::fan_out` /
//! `flush_pending_snapshots`). Still no head-of-line stall: the retry is
//! per-consumer, not a link-wide await.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, BootstrapStreamProfile};
use phux_protocol::ids::{BootstrapId, GroupId, SatelliteHost, StreamId, TerminalId};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnError, SpawnResult,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, trace, warn};

use crate::state::{ClientId, Outbound};

/// Capacity of each per-satellite relay mailbox. Small and bounded: the
/// link is a single ordered stream, so queueing more than a burst behind
/// it only adds latency. Producers `try_send` and fail fast on `Full`.
pub(crate) const RELAY_MAILBOX: usize = 64;
/// Relay retention policy: one generation may carry at most 16 MiB / 128
/// frames; one slow subscriber may retain at most 2 MiB / 16 frames; and one
/// satellite connection may retain at most 8 MiB / 64 frames across all
/// subscribers. These are deliberately independent of the negotiated
/// per-frame ceiling: multiplying that ceiling by a large chunk count made a
/// single hostile generation a gigabyte-scale memory commitment.
const MAX_RELAY_GENERATION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RELAY_GENERATION_FRAMES: u32 = 128;
const MAX_RELAY_SUBSCRIBER_RETAINED_BYTES: usize = 2 * 1024 * 1024;
const MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES: usize = 16;
const MAX_RELAY_CONNECTION_RETAINED_BYTES: usize = 8 * 1024 * 1024;
const MAX_RELAY_CONNECTION_RETAINED_FRAMES: usize = 64;
const RETAINED_FRAME_OVERHEAD: usize = 256;

/// Upper bound on one relayed command round trip, measured at the
/// consumer-facing [`RelayHandle::command`]. Deliberately equal to the
/// transport idle timeout (`phux-dial`'s QUIC `max_idle_timeout`, mirrored
/// by the WS keepalive in `super::link`): a link that dies loudly resolves
/// in-flight commands through session teardown well before this fires, so
/// the deadline is the backstop for the quiet failures — a partition the
/// transport has not noticed yet, or a satellite that reads frames but
/// never answers. Elapsing resolves to a typed `SatelliteUnreachable`
/// error, never an indefinite wait (L1 §9.1).
pub(crate) const RELAY_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// One hub-side consumer's registration on the return leg of a link:
/// which satellite-local terminal it observes and where re-tagged frames
/// for it should land.
#[derive(Debug)]
pub(crate) struct ProxySubscription {
    /// Satellite-local terminal id (the `id` of `Satellite { host, id }`).
    pub(crate) terminal: u32,
    /// Hub-side client identity, for teardown on detach.
    pub(crate) client: ClientId,
    /// The client's outbound mailbox.
    pub(crate) out_tx: mpsc::Sender<Outbound>,
    /// Monotonic ordering token stamped by [`RelayHandle`] at enqueue
    /// (phux-v45.7 reorder guard). A registration and its later
    /// withdrawal ride *different* channels — the bounded request mailbox
    /// vs. the unbounded unsubscribe channel — which the link session's
    /// `select!` may drain in either order. Carrying the issue order lets
    /// the session tell a fresh re-attach (higher token) from a stale
    /// detach (lower token) so a detach-then-reattach of the same
    /// `(client, terminal)` cannot silently tear the re-attach down.
    /// Producers set this via `RelayHandle::next_seq`; direct session-test
    /// construction sets it explicitly.
    pub(crate) seq: u64,
    /// Whether this registration establishes a *content* stream that opens
    /// with a return-leg `TERMINAL_SNAPSHOT` — i.e. it rode a relayed
    /// `ATTACH_TERMINAL` (phux-v45.14). When `true`, the freshly-registered
    /// subscriber starts gated: its content deltas are suppressed until its
    /// own snapshot lands (L1 §9.1 snapshot-precedes-delta), because a
    /// second consumer attaching to a terminal already streaming to another
    /// consumer would otherwise observe that ongoing stream's
    /// `TERMINAL_OUTPUT` before its own snapshot arrives ~1 RTT later.
    /// `false` for event-only subscriptions (`SUBSCRIBE_TERMINAL_EVENTS`,
    /// `SUBSCRIBE_EVENTS`): those carry no snapshot, so their `EVENT` deltas
    /// must flow immediately and gating them would strand the subscriber.
    pub(crate) awaits_snapshot: bool,
    /// Exact bootstrap selection of the downstream consumer connection.
    /// Content subscriptions require both values and must match the
    /// satellite link exactly; event-only subscriptions may carry `None`
    /// because they never receive terminal content bytes.
    pub(crate) bootstrap_profile: Option<BootstrapProfile>,
    /// Exact per-frame bounds selected by the downstream connection.
    pub(crate) bootstrap_limits: Option<BootstrapLimits>,
}

/// A subscription-withdrawal request, carried on the relay's dedicated
/// **unbounded** unsubscribe channel (phux-v45.11 finding 1): teardown
/// must never be droppable under mailbox pressure, or a detached
/// consumer's `ProxySubscriber` entry outlives it and every future
/// return-leg frame is `try_send`-ed into a dead mailbox. Unbounded is
/// safe here — at most a handful per consumer disconnect.
#[derive(Debug)]
pub(crate) enum Unsubscribe {
    /// Drop every proxy subscription `ClientId` holds on this link
    /// (consumer detach / disconnect).
    Client(ClientId),
    /// Drop one client's subscription to one satellite-local terminal
    /// (the relayed `DETACH_TERMINAL` path, phux-v45.7).
    Terminal {
        /// The unsubscribing hub-side client.
        client: ClientId,
        /// The satellite-local terminal id it stops observing.
        terminal: u32,
        /// Ordering token this withdrawal was issued with (see
        /// [`ProxySubscription::seq`]). The session applies the
        /// withdrawal only when no registration with a *newer* token
        /// exists — otherwise a same-`(client, terminal)` re-attach that
        /// the request mailbox delivered first would be torn down by this
        /// stale detach.
        seq: u64,
        /// Resolves after the link removes the proxy. Dropped by disconnected
        /// supervisor drains, where no proxy registry exists to withdraw from.
        reply: Option<WithdrawalReceipt>,
    },
}

/// Serializes a timeout's cancellation with the link's synchronous mutation.
/// A completed result wins even if its waiting task has not been polled yet.
/// No lock is held across an await or an upstream write.
#[derive(Debug)]
pub(crate) struct WithdrawalReceipt {
    reply: oneshot::Sender<CommandResult>,
    outcome: Arc<Mutex<Option<CommandResult>>>,
}

impl WithdrawalReceipt {
    fn new(reply: oneshot::Sender<CommandResult>) -> Self {
        Self {
            reply,
            outcome: Arc::new(Mutex::new(None)),
        }
    }

    /// Apply exactly once unless timeout already committed a safe refusal.
    pub(crate) fn apply(
        self,
        withdraw: impl FnOnce() -> (CommandResult, Vec<Vec<u8>>),
    ) -> Vec<Vec<u8>> {
        let mut outcome = lock_withdrawal_outcome(&self.outcome);
        if outcome.is_some() {
            return Vec::new();
        }
        let (result, frames) = withdraw();
        *outcome = Some(result.clone());
        drop(outcome);
        let _ = self.reply.send(result);
        frames
    }
}

#[expect(
    clippy::expect_used,
    reason = "a panic during withdrawal can leave partial mutation; never recover a safe refusal from a poisoned decision"
)]
fn lock_withdrawal_outcome(
    outcome: &Mutex<Option<CommandResult>>,
) -> std::sync::MutexGuard<'_, Option<CommandResult>> {
    outcome.lock().expect("withdrawal outcome poisoned")
}

/// Validated spawn payload for one satellite. The runtime checks the owner's
/// host before reducing its address to a satellite-local numeric id.
#[derive(Debug)]
pub(crate) struct SatelliteSpawn {
    /// Group under which the satellite spawns (validated there).
    pub(crate) group: GroupId,
    /// Command + argv, or the satellite's default shell.
    pub(crate) command: Option<Vec<String>>,
    /// Working directory on the satellite, or its default policy.
    pub(crate) cwd: Option<String>,
    /// Environment pairs added to the satellite's inherited environment.
    pub(crate) env: Option<Vec<(String, String)>>,
    /// First-class `TERM` override.
    pub(crate) term: Option<String>,
    /// Exact window owner, reduced to a local id after host validation.
    pub(crate) owner_terminal: Option<u32>,
    /// Initial grid and PTY dimensions requested by the consumer.
    pub(crate) initial_size: Option<(u16, u16)>,
    /// Kind, parent, and agent-session provenance, with the parent already
    /// reduced to the satellite's `Local` space (ADR-0104 §6). `None` for
    /// the plain Terminal spawn every pre-kinds consumer sends.
    pub(crate) resource: Option<Box<phux_protocol::wire::frame::SpawnResource>>,
}

/// A request from a hub-side consumer path to one satellite's relay.
#[derive(Debug)]
pub(crate) enum RelayRequest {
    /// Relay a `COMMAND` whose terminal ids are already rewritten to the
    /// satellite's `Local` space. The session allocates the link-side
    /// `request_id` and resolves `reply` with the correlated
    /// `COMMAND_RESULT` (or a typed error on disconnect).
    Command {
        /// The command to forward, ids already satellite-local.
        command: Command,
        /// Resolved with the satellite's result; dropping the receiver is
        /// legal (detached fire-and-forget relays do exactly that).
        reply: oneshot::Sender<CommandResult>,
        /// A proxy subscription to register **atomically with** the
        /// command enqueue (phux-v45.11 finding 2): either the command
        /// goes on the wire and the hub-side registration exists, or
        /// neither happens. Rolled back if the satellite answers with an
        /// error (finding 3) — an errored subscribe took no effect
        /// satellite-side, so the hub must not keep fanning to a consumer
        /// the satellite will never feed.
        subscribe: Option<ProxySubscription>,
    },
    /// Relay a fire-and-forget frame (`INPUT_*`, `FRAME_ACK`,
    /// `TERMINAL_RESIZE`), terminal ids already rewritten satellite-local.
    /// No reply; a dead link drops it (with the teardown notification
    /// covering the observable side).
    Forward {
        /// The frame to forward verbatim.
        frame: FrameKind,
    },
    /// Relay a `SPAWN_TERMINAL` to this satellite (phux-v45.6, L1 §3.1 /
    /// §9.1). Like [`Self::Command`] the session allocates the link-side
    /// `request_id` (the spawn shares the pending id space) and resolves
    /// `reply` with the correlated `TERMINAL_SPAWNED.result`, the freshly
    /// allocated id re-tagged `Local -> Satellite { host, id }`. The
    /// frame put on the wire carries `satellite: None` — the satellite
    /// spawns locally; hub-and-spoke never chains.
    Spawn {
        /// Spawn fields, with owner addressing already validated.
        spawn: SatelliteSpawn,
        /// Resolved with the re-tagged spawn result (or a typed
        /// `SpawnError` on disconnect / timeout).
        reply: oneshot::Sender<SpawnResult>,
    },
    /// Register a proxy subscription AND put `forward` on the wire, as
    /// one atomic step (phux-v45.11 finding 2). Used by the satellite-
    /// scoped `SUBSCRIBE_EVENTS` path, whose forward has no reply frame:
    /// if this request cannot be enqueued, the caller pushes a typed
    /// error to the consumer and nothing is registered anywhere.
    /// Idempotent per `(terminal, client)`.
    Subscribe {
        /// The consumer registration.
        subscription: ProxySubscription,
        /// The frame to forward to the satellite in the same step
        /// (already rewritten satellite-local).
        forward: FrameKind,
    },
}

/// The receiving half of one satellite's relay: the bounded request
/// mailbox plus the unbounded unsubscribe channel, both drained by the
/// link supervisor ([`super::link::run_link`]).
#[derive(Debug)]
pub(crate) struct RelayMailbox {
    /// Bounded consumer-request mailbox ([`RELAY_MAILBOX`]).
    pub(crate) requests: mpsc::Receiver<RelayRequest>,
    /// Unbounded, undroppable subscription teardown (phux-v45.11).
    pub(crate) unsubscribes: mpsc::UnboundedReceiver<Unsubscribe>,
}

/// Cheaply-cloneable producer handle to one satellite's relay mailbox.
#[derive(Debug, Clone)]
pub(crate) struct RelayHandle {
    host: SatelliteHost,
    tx: mpsc::Sender<RelayRequest>,
    unsub_tx: mpsc::UnboundedSender<Unsubscribe>,
    /// Shared monotonic source of the ordering token every proxy
    /// registration and terminal unsubscribe carries (see
    /// [`ProxySubscription::seq`]). One counter per link, shared across
    /// every `RelayHandle` clone for the host, so all of that host's
    /// subscribe/detach operations are totally ordered by issue time
    /// regardless of which mailbox they ride.
    seq: Arc<AtomicU64>,
}

impl RelayHandle {
    /// Pair a fresh handle with the mailbox its link supervisor drains.
    pub(crate) fn new(host: SatelliteHost) -> (Self, RelayMailbox) {
        let (tx, rx) = mpsc::channel(RELAY_MAILBOX);
        let (unsub_tx, unsub_rx) = mpsc::unbounded_channel();
        (
            Self {
                host,
                tx,
                unsub_tx,
                seq: Arc::new(AtomicU64::new(1)),
            },
            RelayMailbox {
                requests: rx,
                unsubscribes: unsub_rx,
            },
        )
    }

    /// Allocate the next issue-order token for a registration or a
    /// terminal withdrawal (phux-v45.7 reorder guard). `Relaxed` is
    /// sufficient: the hub runs on a single-threaded `LocalSet`
    /// (ADR-0014), so all allocations are already program-ordered; the
    /// atomic only needs to hand out distinct, increasing values.
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// The satellite host this handle relays to (aggregation callers
    /// re-tag return-leg ids with it — phux-v45.5).
    pub(crate) const fn host(&self) -> &SatelliteHost {
        &self.host
    }

    /// Relay `command` and await the correlated result. Fails fast — a
    /// saturated mailbox, a dead link task, or a link lost mid-flight all
    /// produce a typed error instead of a hang (the session and the link
    /// supervisor's drain phases guarantee the oneshot always resolves or
    /// drops promptly) — and fails *bounded* even when nothing else does:
    /// [`RELAY_COMMAND_TIMEOUT`] caps the wait against a silently
    /// partitioned or frame-swallowing satellite whose link still looks
    /// `Connected`. This is the whole-connection safety valve: the caller
    /// (`handle_command`) is awaited inline in the consumer's read loop,
    /// so an unbounded wait here would wedge every subsequent frame from
    /// that consumer. Timing out drops the oneshot receiver, which marks
    /// the session's pending entry for pruning
    /// ([`RelaySession::prune_abandoned`]).
    pub(crate) async fn command(&self, command: Command) -> CommandResult {
        self.command_inner(command, None).await
    }

    /// Relay `command` and register `subscription` atomically with its
    /// enqueue (phux-v45.11 finding 2): a request that never reaches the
    /// link registers nothing, and the session rolls the registration
    /// back if the satellite answers with an error (finding 3). This is
    /// the path for commands that establish a return-leg stream —
    /// `SUBSCRIBE_TERMINAL_EVENTS` and `ATTACH_TERMINAL` (phux-v45.7).
    pub(crate) async fn command_subscribing(
        &self,
        command: Command,
        subscription: ProxySubscription,
    ) -> CommandResult {
        self.command_inner(command, Some(subscription)).await
    }

    async fn command_inner(
        &self,
        command: Command,
        subscribe: Option<ProxySubscription>,
    ) -> CommandResult {
        // Stamp the registration's issue-order token before it can race a
        // later detach across the channel split (phux-v45.7).
        let subscribe = subscribe.map(|mut sub| {
            sub.seq = self.next_seq();
            sub
        });
        let (reply, rx) = oneshot::channel();
        match self.tx.try_send(RelayRequest::Command {
            command,
            reply,
            subscribe,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                return CommandResult::Error {
                    code: ErrorCode::ResourceExhausted,
                    message: format!("satellite {} link is saturated; retry", self.host),
                };
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message: format!("satellite {} link is down", self.host),
                };
            }
        }
        match tokio::time::timeout(RELAY_COMMAND_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: format!("satellite {} link dropped before the reply", self.host),
            },
            Err(_) => CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: format!(
                    "satellite {} did not answer within {}s",
                    self.host,
                    RELAY_COMMAND_TIMEOUT.as_secs()
                ),
            },
        }
    }

    /// Relay a `SPAWN_TERMINAL` and await the correlated re-tagged
    /// `SpawnResult` (phux-v45.6). The same fail-fast / bounded contract
    /// as [`Self::command`], expressed in the spawn reply's own typed
    /// error vocabulary: a saturated mailbox is `SpawnFailed` (retryable,
    /// the link is up), a dead or unanswering link is
    /// `SatelliteUnreachable`. Timing out drops the oneshot receiver,
    /// which marks the pending entry for [`RelaySession::prune_abandoned`].
    pub(crate) async fn spawn(&self, spawn: SatelliteSpawn) -> SpawnResult {
        let (reply, rx) = oneshot::channel();
        match self.tx.try_send(RelayRequest::Spawn { spawn, reply }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                return SpawnResult::Err(SpawnError::SpawnFailed(format!(
                    "satellite {} link is saturated; retry",
                    self.host
                )));
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return SpawnResult::Err(SpawnError::SatelliteUnreachable(format!(
                    "satellite {} link is down",
                    self.host
                )));
            }
        }
        match tokio::time::timeout(RELAY_COMMAND_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => SpawnResult::Err(SpawnError::SatelliteUnreachable(format!(
                "satellite {} link dropped before the spawn reply",
                self.host
            ))),
            Err(_) => SpawnResult::Err(SpawnError::SatelliteUnreachable(format!(
                "satellite {} did not answer the spawn within {}s",
                self.host,
                RELAY_COMMAND_TIMEOUT.as_secs()
            ))),
        }
    }

    /// Relay `command` without awaiting the result (the idempotent batch
    /// path — `KILL_TERMINALS` semantics tolerate a silent skip).
    pub(crate) fn command_detached(&self, command: Command) {
        let (reply, _rx) = oneshot::channel();
        if self
            .tx
            .try_send(RelayRequest::Command {
                command,
                reply,
                subscribe: None,
            })
            .is_err()
        {
            debug!(satellite = %self.host, "detached satellite command dropped (link down or saturated)");
        }
    }

    /// Relay a fire-and-forget frame. Drops with a warn on a saturated or
    /// dead link — the same contract those frames already have locally.
    pub(crate) fn forward(&self, frame: FrameKind) {
        if let Err(err) = self.tx.try_send(RelayRequest::Forward { frame }) {
            warn!(
                satellite = %self.host,
                reason = %trysend_reason(&err),
                "satellite relay frame dropped (fire-and-forget)"
            );
        }
    }

    /// Register a proxy subscription and forward `forward` to the
    /// satellite, atomically (see [`RelayRequest::Subscribe`]). On a
    /// saturated or dead link **nothing** is registered and the consumer
    /// gets a typed `ERROR` push instead of silence (phux-v45.11
    /// finding 2 — `SUBSCRIBE_EVENTS` has no reply frame to carry the
    /// failure, so the push is the only observable channel).
    pub(crate) fn subscribe(&self, mut subscription: ProxySubscription, forward: FrameKind) {
        subscription.seq = self.next_seq();
        let consumer = subscription.out_tx.clone();
        let terminal = subscription.terminal;
        if let Err(err) = self.tx.try_send(RelayRequest::Subscribe {
            subscription,
            forward,
        }) {
            let (code, message) = match err {
                mpsc::error::TrySendError::Full(_) => (
                    ErrorCode::ResourceExhausted,
                    format!("satellite {} link is saturated; retry", self.host),
                ),
                mpsc::error::TrySendError::Closed(_) => (
                    ErrorCode::SatelliteUnreachable,
                    format!("satellite {} link is down", self.host),
                ),
            };
            warn!(
                satellite = %self.host,
                terminal,
                %message,
                "satellite proxy subscription refused; notifying consumer"
            );
            let _ = consumer.try_send(Outbound::Frame(FrameKind::Error {
                request_id: None,
                code,
                message,
            }));
        }
    }

    /// Drop every proxy subscription `client` holds on this link.
    /// Undroppable (phux-v45.11 finding 1): rides the unbounded
    /// unsubscribe channel, so mailbox pressure can never leave a stale
    /// `ProxySubscriber` behind. If the link task is gone its registry is
    /// gone too — the send error is then meaningless.
    pub(crate) fn unsubscribe_client(&self, client: ClientId) {
        let _ = self.unsub_tx.send(Unsubscribe::Client(client));
    }

    /// Drop `client`'s subscription to one satellite-local terminal
    /// (the relayed `DETACH_TERMINAL` path, phux-v45.7). Same undroppable
    /// channel as [`Self::unsubscribe_client`]. Waits for proxy withdrawal
    /// before success; a stalled link returns a bounded error instead.
    pub(crate) async fn unsubscribe_terminal(
        &self,
        client: ClientId,
        terminal: u32,
    ) -> CommandResult {
        let seq = self.next_seq();
        let (reply, received) = oneshot::channel();
        let reply = WithdrawalReceipt::new(reply);
        let outcome = Arc::clone(&reply.outcome);
        let _ = self.unsub_tx.send(Unsubscribe::Terminal {
            client,
            terminal,
            seq,
            reply: Some(reply),
        });
        // A dropped receipt means the supervisor has no live session (refused,
        // connecting, backing off, or shut down), hence no proxy can fan out.
        // Timeout cancels an unapplied withdrawal under the same lock used by
        // application. If application already won, return its actual result.
        match tokio::time::timeout(RELAY_COMMAND_TIMEOUT, received).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => CommandResult::Ok,
            Err(_) => lock_withdrawal_outcome(&outcome).get_or_insert_with(|| CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: format!(
                    "satellite {} proxy withdrawal cancelled before application after {}s; subscription preserved",
                    self.host,
                    RELAY_COMMAND_TIMEOUT.as_secs()
                ),
            }).clone(),
        }
    }
}

const fn trysend_reason(err: &mpsc::error::TrySendError<RelayRequest>) -> &'static str {
    match err {
        mpsc::error::TrySendError::Full(_) => "mailbox full",
        mpsc::error::TrySendError::Closed(_) => "link task gone",
    }
}

/// Shared registry of per-satellite [`RelayHandle`]s, mirrored into
/// server state at hub bring-up (the sibling of
/// [`super::link::HubLinkStatuses`]). Empty on a non-hub server.
#[derive(Debug, Clone, Default)]
pub(crate) struct HubRelays {
    inner: Arc<Mutex<BTreeMap<SatelliteHost, RelayHandle>>>,
}

impl HubRelays {
    /// Register `handle` for its host (hub bring-up, one per table entry).
    pub(crate) fn insert(&self, handle: RelayHandle) {
        self.lock().insert(handle.host.clone(), handle);
    }

    /// The relay handle for `host`, if the hub dials that satellite.
    pub(crate) fn get(&self, host: &SatelliteHost) -> Option<RelayHandle> {
        self.lock().get(host).cloned()
    }

    /// Every registered handle (detach fan-out).
    pub(crate) fn all(&self) -> Vec<RelayHandle> {
        self.lock().values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<SatelliteHost, RelayHandle>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Fail one queued request while the link is not connected (dial in
/// flight, backoff, fail-closed refusal). Used by the link supervisor's
/// drain arms so a consumer never hangs on a dead satellite.
pub(crate) fn fail_fast(request: RelayRequest, host: &SatelliteHost, why: &str) {
    match request {
        // A command's atomic `subscribe` rider registers nothing here:
        // the request never reached a session, so failing the oneshot is
        // the whole story (the consumer sees the typed error reply).
        RelayRequest::Command { reply, .. } => {
            let _ = reply.send(CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: format!("satellite {host} is unreachable: {why}"),
            });
        }
        RelayRequest::Spawn { reply, .. } => {
            let _ = reply.send(SpawnResult::Err(SpawnError::SatelliteUnreachable(format!(
                "satellite {host} is unreachable: {why}"
            ))));
        }
        RelayRequest::Forward { frame } => {
            trace!(satellite = %host, kind = ?frame_label(&frame), why, "relay frame dropped while disconnected");
        }
        RelayRequest::Subscribe { subscription, .. } => {
            // A subscription to an unreachable satellite gets the same
            // typed notification a disconnect would produce — observable,
            // not silence (SUBSCRIBE_EVENTS has no reply frame). Nothing
            // was registered, so there is nothing to roll back.
            let _ = subscription
                .out_tx
                .try_send(Outbound::Frame(unreachable_error(host, why)));
            trace!(
                satellite = %host,
                client = ?subscription.client,
                why,
                "proxy subscription refused while disconnected"
            );
        }
    }
}

/// The typed `ERROR` frame consumers receive when a satellite they observe
/// (or tried to observe) is unreachable.
fn unreachable_error(host: &SatelliteHost, why: &str) -> FrameKind {
    FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: format!("satellite {host} is unreachable: {why}"),
    }
}

/// A payload-free label for logging a relayed frame.
const fn frame_label(frame: &FrameKind) -> &'static str {
    match frame {
        FrameKind::InputKey { .. } => "INPUT_KEY",
        FrameKind::InputMouse { .. } => "INPUT_MOUSE",
        FrameKind::InputFocus { .. } => "INPUT_FOCUS",
        FrameKind::InputPaste { .. } => "INPUT_PASTE",
        FrameKind::FrameAck { .. } => "FRAME_ACK",
        FrameKind::TerminalResize { .. } => "TERMINAL_RESIZE",
        FrameKind::SubscribeEvents { .. } => "SUBSCRIBE_EVENTS",
        _ => "other",
    }
}

/// One proxy subscriber on the return leg.
#[derive(Debug)]
struct ProxySubscriber {
    client: ClientId,
    out_tx: mpsc::Sender<Outbound>,
    /// Issue-order token of the registration currently held for this
    /// `(terminal, client)` (see [`ProxySubscription::seq`]). Compared
    /// against a terminal withdrawal's token so a stale detach cannot
    /// tear down a newer re-attach.
    seq: u64,
    /// This subscriber's L1 §9.1 snapshot-ordering gate (phux-v45.12 /
    /// phux-v45.14). Content deltas (`TERMINAL_OUTPUT`) are held back until
    /// the subscriber's own `TERMINAL_SNAPSHOT` has been delivered, so a
    /// delta can never overtake the snapshot across the two-hop attach. See
    /// [`SnapshotGate`].
    gate: SnapshotGate,
}

/// The per-subscriber snapshot-ordering gate on the return leg (L1 §9.1,
/// "the snapshot MUST precede the first delta"). A subscriber may only
/// receive content deltas once its own attach `TERMINAL_SNAPSHOT` has
/// landed; this enum is the non-blocking mirror of the local attach's
/// snapshot gate, holding the ordering guarantee without stalling the link
/// for one slow consumer.
#[derive(Debug)]
enum SnapshotGate {
    /// The subscriber attached (a relayed `ATTACH_TERMINAL`) but its own
    /// return-leg snapshot has not been delivered yet (phux-v45.14). Deltas
    /// are suppressed: a second consumer attaching to a terminal already
    /// streaming to another consumer must not observe that ongoing stream's
    /// `TERMINAL_OUTPUT` before its own snapshot arrives ~1 RTT later. The
    /// first snapshot to fan out (its attach snapshot) clears this to
    /// [`Self::Open`], or, if the mailbox refuses it, to [`Self::Retained`].
    AwaitingFirst,
    /// The subscriber's snapshot has been delivered (or it is an event-only
    /// subscription that carries no snapshot): deltas flow normally.
    Open,
    /// Ordered bootstrap frames refused by a briefly-full consumer mailbox.
    /// The queue is bounded independently per subscriber and across the
    /// satellite connection; exceeding either budget reaps the subscriber.
    Retained {
        frames: VecDeque<FrameKind>,
        retained_bytes: usize,
        open_after: bool,
    },
}

/// One in-flight relayed command: the waiting consumer plus, when the
/// command carried an atomic subscription rider, what to roll back if
/// the satellite answers with an error (phux-v45.11 finding 3).
#[derive(Debug)]
struct PendingCommand {
    reply: oneshot::Sender<CommandResult>,
    /// `(terminal, client, effect)` — the registration effect this command's
    /// subscription rider had, so a satellite error undoes exactly what it did
    /// (phux-v45.11 finding 3, phux-v45.15). See [`Registration`].
    subscription: Option<(u32, ClientId, Registration)>,
}

/// What registering a subscription rider did, remembered so a satellite error
/// can roll back precisely (phux-v45.11 finding 3, phux-v45.15).
#[derive(Debug, Clone, Copy)]
enum Registration {
    /// A brand-new `(terminal, client)` subscriber was pushed. An error
    /// removes it.
    New,
    /// An idempotent re-subscribe that **re-gated** an already-`Open`
    /// subscriber to `AwaitingFirst` because it upgraded to a
    /// snapshot-bearing attach (phux-v45.15). The attach's own snapshot
    /// re-opens the gate — but a satellite error means that snapshot never
    /// comes, so the error must restore the gate to `Open` **if it is still
    /// `AwaitingFirst`**, or the pre-existing (event-only, or already-attached
    /// and snapshot-landed) stream is stranded behind a gate that never
    /// opens. A snapshot retained while the command was in flight supersedes
    /// this rollback and must remain gated until it is delivered.
    Regated,
    /// An idempotent re-subscribe that left the existing gate untouched (the
    /// pre-existing registration belongs to an earlier successful subscribe
    /// and must survive). An error rolls back nothing.
    Idempotent,
}

/// The per-connection relay state a link supervisor drives while its
/// satellite connection is up (see [`super::link::run_link`]).
///
/// Owns the link-side `request_id` allocation, the pending-command map,
/// and the proxy-subscription registry. All state is session-scoped:
/// [`Self::teardown`] fails pending commands and notifies subscribers, so
/// a reconnected link starts clean (consumers re-issue and re-subscribe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RelayBootstrapFlow {
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    next_chunk_seq: u32,
    generation_bytes: u64,
    generation_frames: u32,
    ready: bool,
}

/// Reject a chunk whose stream/bootstrap identity drifted away from the
/// in-flight generation, or that arrives after that generation reached READY.
fn ensure_chunk_identity(
    host: &SatelliteHost,
    terminal: u32,
    flow: &RelayBootstrapFlow,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
) -> Result<(), String> {
    if flow.ready || flow.stream_id != stream_id || flow.bootstrap_id != bootstrap_id {
        return Err(format!(
            "satellite {host} changed or reused bootstrap identity before READY for terminal {terminal}"
        ));
    }
    Ok(())
}

/// Reject a chunk that skipped or repeated the generation's chunk sequence.
fn ensure_chunk_sequence(
    host: &SatelliteHost,
    flow: &RelayBootstrapFlow,
    chunk_seq: u32,
) -> Result<(), String> {
    if chunk_seq != flow.next_chunk_seq {
        return Err(format!(
            "satellite {host} sent BOOTSTRAP_CHUNK sequence {chunk_seq}, expected {}",
            flow.next_chunk_seq
        ));
    }
    Ok(())
}

/// One more frame charged against `limit`, or the rejection a satellite gets
/// for outrunning `budget` (a spent budget and an overflowed counter are the
/// same refusal).
fn charge_frame(
    host: &SatelliteHost,
    budget: &str,
    current: u32,
    limit: u32,
) -> Result<u32, String> {
    current
        .checked_add(1)
        .filter(|count| *count <= limit)
        .ok_or_else(|| format!("satellite {host} exceeded the {budget}"))
}

/// `payload_len` more bytes charged against `limit`, or the rejection a
/// satellite gets for outrunning `budget`.
fn charge_bytes(
    host: &SatelliteHost,
    budget: &str,
    current: u64,
    payload_len: u64,
    limit: u64,
) -> Result<u64, String> {
    current
        .checked_add(payload_len)
        .filter(|bytes| *bytes <= limit)
        .ok_or_else(|| format!("satellite {host} exceeded the {budget}"))
}

/// The terminal scope a return-leg stream frame carries, if it has one.
const fn stream_frame_scope(frame: &FrameKind) -> Option<&TerminalId> {
    match frame {
        FrameKind::Event { terminal, .. } => terminal.as_ref(),
        FrameKind::TerminalOutput { terminal_id, .. }
        | FrameKind::BootstrapBegin { terminal_id, .. }
        | FrameKind::BootstrapChunk { terminal_id, .. }
        | FrameKind::BootstrapReady { terminal_id, .. }
        | FrameKind::HistoryPage { terminal_id, .. }
        | FrameKind::BootstrapTombstone { terminal_id, .. }
        | FrameKind::HistoryTombstone { terminal_id, .. }
        | FrameKind::HistoryRejected { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}
#[derive(Debug)]
struct PendingDetach {
    terminal: u32,
    deadline: tokio::time::Instant,
}

#[derive(Debug)]
pub(crate) struct RelaySession {
    host: SatelliteHost,
    bootstrap_limits: BootstrapLimits,
    bootstrap_profile: BootstrapProfile,
    next_request_id: u32,
    pending: HashMap<u32, PendingCommand>,
    enforce_bootstrap_flow: bool,
    /// Relayed `SPAWN_TERMINAL`s awaiting their `TERMINAL_SPAWNED`
    /// (phux-v45.6). Shares the link-side `request_id` space with
    /// [`Self::pending`] so one allocator covers both reply frames.
    pending_spawns: HashMap<u32, oneshot::Sender<SpawnResult>>,
    /// Upstream detach barriers. Old frames may still arrive until the
    /// correlated reply, including after a downstream proxy has reattached.
    pending_detaches: HashMap<u32, PendingDetach>,
    subscribers: HashMap<u32, Vec<ProxySubscriber>>,
    /// An explicit content attach has been forwarded since the last upstream
    /// detach. Event-only proxies do not establish this ownership. Even a
    /// refused attach cannot resurrect the automatic SPAWN generation that
    /// its preflight barrier retired.
    explicit_content: HashSet<u32>,
    /// Legacy `SUBSCRIBE_EVENTS` is removed by upstream `DETACH_TERMINAL`, unlike
    /// actor-level `SUBSCRIBE_TERMINAL_EVENTS`. Restore it after internal cuts.
    legacy_events: HashSet<u32>,
    bootstrap_flows: HashMap<u32, RelayBootstrapFlow>,
    retained_bytes: usize,
    retained_frames: usize,
    inflight_generation_bytes: u64,
    inflight_generation_frames: u32,
    encode_buf: BytesMut,
}

impl RelaySession {
    /// Fresh session state for one established connection to `host`.
    #[cfg(test)]
    pub(crate) fn new(host: SatelliteHost, bootstrap_limits: BootstrapLimits) -> Self {
        let mut session =
            Self::new_negotiated(host, bootstrap_limits, BootstrapProfile::SynthesizedVtRaw);
        session.enforce_bootstrap_flow = false;
        session
    }

    /// Fresh session state pinned to the connection's exact negotiated profile.
    pub(crate) fn new_negotiated(
        host: SatelliteHost,
        bootstrap_limits: BootstrapLimits,
        bootstrap_profile: BootstrapProfile,
    ) -> Self {
        Self {
            host,
            bootstrap_limits,
            bootstrap_profile,
            next_request_id: 1,
            pending: HashMap::new(),
            pending_spawns: HashMap::new(),
            pending_detaches: HashMap::new(),
            enforce_bootstrap_flow: true,
            subscribers: HashMap::new(),
            explicit_content: HashSet::new(),
            legacy_events: HashSet::new(),
            bootstrap_flows: HashMap::new(),
            retained_bytes: 0,
            retained_frames: 0,
            inflight_generation_bytes: 0,
            inflight_generation_frames: 0,
            encode_buf: BytesMut::with_capacity(1024),
        }
    }

    /// A first explicit attach must start after any automatically published
    /// SPAWN generation on the link has stopped. No downstream proxy owns that
    /// generation. Fence it upstream before forwarding the attach; peers that
    /// already share a subscription must keep their stream throughout.
    pub(crate) fn prepare_request(&mut self, request: &RelayRequest) -> Vec<Vec<u8>> {
        let RelayRequest::Command {
            command: Command::AttachTerminal { .. },
            subscribe: Some(sub),
            ..
        } = request
        else {
            return Vec::new();
        };
        if self.explicit_content.contains(&sub.terminal)
            || self
                .pending_detaches
                .values()
                .any(|pending| pending.terminal == sub.terminal)
            || self.subscription_rejection(sub).is_some()
        {
            return Vec::new();
        }
        self.prepare_content_cut(sub.terminal)
    }

    fn prepare_content_cut(&mut self, terminal: u32) -> Vec<Vec<u8>> {
        let restore_events = self.legacy_events.contains(&terminal);
        let mut frames = vec![self.encode_terminal_detach(terminal)];
        if restore_events {
            self.legacy_events.insert(terminal);
            frames.push(self.encode(&FrameKind::SubscribeEvents {
                terminal: Some(TerminalId::local(terminal)),
            }));
        }
        frames
    }

    /// Service one consumer request. `None` means the request was rejected
    /// locally before registration and its consumer was already notified.
    pub(crate) fn handle_request_checked(&mut self, request: RelayRequest) -> Option<Vec<u8>> {
        match request {
            RelayRequest::Command {
                command,
                reply,
                subscribe,
            } => {
                if let Some(sub) = subscribe.as_ref()
                    && let Some((code, message)) = self.subscription_rejection(sub)
                {
                    let _ = reply.send(CommandResult::Error { code, message });
                    return None;
                }
                // Register the subscription rider in the same step as the
                // command enqueue (phux-v45.11 finding 2), remembering
                // enough to roll it back on an error reply (finding 3).
                let subscription = subscribe.map(|sub| {
                    let (terminal, client) = (sub.terminal, sub.client);
                    if sub.awaits_snapshot {
                        self.explicit_content.insert(terminal);
                    }
                    let effect = self.register_subscriber(sub);
                    (terminal, client, effect)
                });
                let request_id = self.allocate_request_id();
                self.pending.insert(
                    request_id,
                    PendingCommand {
                        reply,
                        subscription,
                    },
                );
                Some(self.encode(&FrameKind::Command {
                    request_id,
                    command,
                }))
            }
            RelayRequest::Forward { frame } => Some(self.encode(&frame)),
            RelayRequest::Spawn { spawn, reply } => {
                let request_id = self.allocate_request_id();
                self.pending_spawns.insert(request_id, reply);
                Some(self.encode(&FrameKind::SpawnTerminal {
                    request_id,
                    group: spawn.group,
                    command: spawn.command,
                    cwd: spawn.cwd,
                    env: spawn.env,
                    term: spawn.term,
                    // The satellite spawns locally: the addressing field
                    // never crosses the link (hub-and-spoke, no chaining).
                    satellite: None,
                    owner_terminal: spawn.owner_terminal.map(TerminalId::local),
                    agent_session: None,
                    initial_size: spawn.initial_size,
                    // The kind and its already-retagged parent cross the
                    // link intact: a satellite-addressed AgentSession spawn
                    // is the satellite's to validate and bind, exactly as a
                    // local one would be (ADR-0104 §6).
                    resource: spawn.resource,
                }))
            }
            RelayRequest::Subscribe {
                subscription,
                forward,
            } => self.subscribe_forward(subscription, &forward),
        }
    }

    /// Register and remember event scope atomically with its forward frame.
    fn subscribe_forward(
        &mut self,
        subscription: ProxySubscription,
        forward: &FrameKind,
    ) -> Option<Vec<u8>> {
        if let Some((code, message)) = self.subscription_rejection(&subscription) {
            let _ = subscription
                .out_tx
                .try_send(Outbound::Frame(FrameKind::Error {
                    request_id: None,
                    code,
                    message,
                }));
            return None;
        }
        if let FrameKind::SubscribeEvents {
            terminal: Some(TerminalId::Local { id }),
        } = forward
        {
            self.legacy_events.insert(*id);
        }
        self.register_subscriber(subscription);
        Some(self.encode(forward))
    }

    #[cfg(test)]
    fn handle_request(&mut self, request: RelayRequest) -> Vec<u8> {
        self.handle_request_checked(request)
            .expect("test request must be forwarded")
    }

    fn subscription_rejection(
        &self,
        subscription: &ProxySubscription,
    ) -> Option<(ErrorCode, String)> {
        if !subscription.awaits_snapshot {
            return None;
        }
        if subscription.bootstrap_profile == Some(self.bootstrap_profile)
            && subscription.bootstrap_limits == Some(self.bootstrap_limits)
        {
            return None;
        }
        Some((
            ErrorCode::CodecUnavailable,
            format!(
                "satellite {} link uses profile {:?} and limits {:?}; downstream content connection selected profile {:?} and limits {:?}; relay transcoding is unavailable",
                self.host,
                self.bootstrap_profile,
                self.bootstrap_limits,
                subscription.bootstrap_profile,
                subscription.bootstrap_limits,
            ),
        ))
    }

    /// Register one proxy subscriber, idempotently. Returns the
    /// [`Registration`] effect so a satellite error can undo exactly what this
    /// did. A brand-new `(terminal, client)` pair is [`Registration::New`]; a
    /// re-subscribe refreshes the stored mailbox and is either
    /// [`Registration::Regated`] (an UPGRADE that re-gated an `Open` stream) or
    /// [`Registration::Idempotent`] (gate left untouched).
    fn register_subscriber(&mut self, subscription: ProxySubscription) -> Registration {
        let ProxySubscription {
            terminal,
            client,
            out_tx,
            seq,
            awaits_snapshot,
            bootstrap_profile: _,
            bootstrap_limits: _,
        } = subscription;
        let subs = self.subscribers.entry(terminal).or_default();
        if let Some(existing) = subs.iter_mut().find(|s| s.client == client) {
            existing.out_tx = out_tx;
            // Advance to the freshest token seen: a re-attach must never
            // regress the stored order below a withdrawal it superseded.
            existing.seq = existing.seq.max(seq);
            // Upgrade re-gating (phux-v45.15): a same-client re-subscribe that
            // upgrades from an event-only stream (or an already-attached,
            // snapshot-landed stream) to a snapshot-bearing attach must
            // re-suppress deltas until *this* attach's own snapshot lands —
            // otherwise the attach's deltas ride ahead of its snapshot on the
            // still-`Open` gate (the L1 §9.1 violation v45.14 fixed for a
            // fresh second attach, resurfacing on the upgrade path). Only an
            // `Open` gate is re-gated: an `AwaitingFirst`/`Retained` gate
            // already suppresses deltas until a snapshot lands, and the fresh
            // attach's snapshot supersedes a retained one (freshest-wins) when
            // it fans out. A non-attach (event-only) re-subscribe carries no
            // snapshot and leaves the gate untouched, so an in-order stream
            // the consumer is already reading is never re-suppressed.
            if awaits_snapshot && matches!(existing.gate, SnapshotGate::Open) {
                existing.gate = SnapshotGate::AwaitingFirst;
                Registration::Regated
            } else {
                Registration::Idempotent
            }
        } else {
            subs.push(ProxySubscriber {
                client,
                out_tx,
                seq,
                // An attach gates until its own snapshot lands (phux-v45.14);
                // an event-only subscription carries no snapshot, so it opens
                // straight away or its EVENT deltas would never flow.
                gate: if awaits_snapshot {
                    SnapshotGate::AwaitingFirst
                } else {
                    SnapshotGate::Open
                },
            });
            Registration::New
        }
    }

    /// Withdraw proxy subscriptions (the undroppable unsubscribe channel,
    /// phux-v45.11 findings 1 and 4). Returns the encoded wire frames to
    /// send to the satellite: one `COMMAND { DETACH_TERMINAL }` per
    /// terminal whose **last** proxy subscriber just went away (or whose
    /// explicit withdrawal found no proxy after an automatic spawn), so the
    /// satellite stops streaming output for terminals nobody on this hub
    /// observes anymore. The upstream reply retires the old bootstrap flow;
    /// frames preceding it cannot reach a newly registered proxy. The
    /// downstream receipt certifies local proxy removal, not the upstream reply.
    pub(crate) fn handle_unsubscribe(&mut self, unsubscribe: Unsubscribe) -> Vec<Vec<u8>> {
        match unsubscribe {
            Unsubscribe::Client(client) => {
                let orphaned = self.withdraw_client(client);
                self.encode_withdrawals(orphaned)
            }
            Unsubscribe::Terminal {
                client,
                terminal,
                seq,
                reply,
            } => match reply {
                Some(reply) => {
                    reply.apply(|| self.apply_terminal_withdrawal(client, terminal, seq))
                }
                None => self.apply_terminal_withdrawal(client, terminal, seq).1,
            },
        }
    }

    fn apply_terminal_withdrawal(
        &mut self,
        client: ClientId,
        terminal: u32,
        seq: u64,
    ) -> (CommandResult, Vec<Vec<u8>>) {
        match self.withdraw_terminal(client, terminal, seq) {
            Ok(orphaned) => (
                CommandResult::Ok,
                self.encode_withdrawals(orphaned.into_iter().collect()),
            ),
            Err(result) => (result, Vec::new()),
        }
    }

    fn encode_withdrawals(&mut self, orphaned: Vec<u32>) -> Vec<Vec<u8>> {
        self.recalculate_retained_totals();
        orphaned
            .into_iter()
            .map(|terminal| self.encode_terminal_detach(terminal))
            .collect()
    }

    fn withdraw_client(&mut self, client: ClientId) -> Vec<u32> {
        let mut orphaned = Vec::new();
        self.subscribers.retain(|terminal, subs| {
            subs.retain(|s| s.client != client);
            if subs.is_empty() {
                orphaned.push(*terminal);
                false
            } else {
                true
            }
        });
        orphaned
    }

    fn withdraw_terminal(
        &mut self,
        client: ClientId,
        terminal: u32,
        seq: u64,
    ) -> Result<Option<u32>, CommandResult> {
        // A relayed SPAWN publishes to the link before any explicit proxy
        // attaches. Idempotent detach must stop that unobserved producer too.
        let Some(subs) = self.subscribers.get_mut(&terminal) else {
            return Ok(Some(terminal));
        };
        // Registrations and withdrawals ride different channels. Never let a
        // stale detach remove a newer registration for the same client.
        if subs.iter().any(|s| s.client == client && s.seq >= seq) {
            debug!(satellite = %self.host, terminal, ?client,
                "stale terminal unsubscribe superseded by a newer re-attach; dropping");
            return Err(CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                message: "terminal detach was superseded by a newer attachment".to_owned(),
            });
        }
        subs.retain(|s| s.client != client);
        if !subs.is_empty() {
            return Ok(None);
        }
        self.subscribers.remove(&terminal);
        Ok(Some(terminal))
    }

    fn encode_terminal_detach(&mut self, terminal: u32) -> Vec<u8> {
        self.explicit_content.remove(&terminal);
        self.legacy_events.remove(&terminal);
        debug!(satellite = %self.host, terminal,
            "last proxy subscriber gone; detaching satellite-side");
        let request_id = self.allocate_request_id();
        self.pending_detaches.insert(
            request_id,
            PendingDetach {
                terminal,
                deadline: tokio::time::Instant::now() + RELAY_COMMAND_TIMEOUT,
            },
        );
        self.encode(&FrameKind::Command {
            request_id,
            command: Command::DetachTerminal {
                terminal_id: TerminalId::local(terminal),
            },
        })
    }

    /// Dispatch one frame arriving from the satellite: resolve relayed
    /// command and spawn replies and re-tag + fan out subscribed streams.
    pub(crate) fn handle_inbound(&mut self, framed: &[u8]) -> Result<(), String> {
        let frame = FrameKind::decode_with_limits(framed, self.bootstrap_limits)
            .map_err(|err| format!("satellite {} sent an undecodable frame: {err:?}", self.host))?
            .0;
        if self.resolve_detach_reply(&frame)? {
            return Ok(());
        }
        match frame {
            FrameKind::CommandResult { request_id, result } => {
                self.resolve_pending(request_id, result);
            }
            FrameKind::TerminalSpawned { request_id, result } => {
                self.resolve_pending_spawn(request_id, result);
            }
            FrameKind::Error {
                request_id: Some(request_id),
                code,
                message,
            } => self.resolve_correlated_error(request_id, code, message),
            FrameKind::Event { .. }
            | FrameKind::TerminalOutput { .. }
            | FrameKind::BootstrapBegin { .. }
            | FrameKind::BootstrapChunk { .. }
            | FrameKind::BootstrapReady { .. }
            | FrameKind::HistoryPage { .. }
            | FrameKind::BootstrapTombstone { .. }
            | FrameKind::HistoryTombstone { .. }
            | FrameKind::HistoryRejected { .. } => self.relay_stream_frame(frame)?,
            FrameKind::TerminalClosed {
                terminal_id,
                exit_status,
                reason,
            } => self.relay_terminal_closed(&terminal_id, exit_status, reason),
            FrameKind::Bell { terminal_id } => self.relay_bell(&terminal_id),
            other => {
                return Err(format!(
                    "satellite {} sent a direction-invalid frame after HELLO_OK: {other:?}",
                    self.host
                ));
            }
        }
        Ok(())
    }

    /// The old upstream generation remains fenced until a successful joined
    /// detach receipt. A refusal cannot authorize a fresh stream on this link.
    fn resolve_detach_reply(&mut self, frame: &FrameKind) -> Result<bool, String> {
        let (request_id, succeeded) = match frame {
            FrameKind::CommandResult { request_id, result } => {
                (*request_id, matches!(result, CommandResult::Ok))
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } => (*request_id, false),
            _ => return Ok(false),
        };
        let Some(pending) = self.pending_detaches.remove(&request_id) else {
            return Ok(false);
        };
        let terminal = pending.terminal;
        if !succeeded {
            return Err(format!(
                "satellite {} refused upstream detach barrier for terminal {terminal}",
                self.host
            ));
        }
        self.retire_bootstrap_flow(terminal);
        Ok(true)
    }

    /// Resolve a correlated `ERROR` against whichever request kind holds the
    /// id — commands own it in the common case, but a satellite MAY answer a
    /// relayed spawn with a generic correlated ERROR instead of
    /// `TERMINAL_SPAWNED`.
    fn resolve_correlated_error(&mut self, request_id: u32, code: ErrorCode, message: String) {
        if self.pending_spawns.contains_key(&request_id) {
            self.resolve_pending_spawn(
                request_id,
                SpawnResult::Err(SpawnError::SpawnFailed(format!(
                    "satellite refused the spawn: {code:?}: {message}"
                ))),
            );
        } else {
            self.resolve_pending(request_id, CommandResult::Error { code, message });
        }
    }

    /// Forward one terminal-scoped return-leg stream frame: resolve its
    /// satellite-local terminal id, clear the bootstrap-flow gate its kind
    /// carries, then re-tag the scope and fan it out to subscribers.
    fn relay_stream_frame(&mut self, frame: FrameKind) -> Result<(), String> {
        let Some(id) = self.retag_inbound(stream_frame_scope(&frame)) else {
            return Ok(());
        };
        if !matches!(frame, FrameKind::Event { .. })
            && self
                .pending_detaches
                .values()
                .any(|pending| pending.terminal == id)
        {
            // These frames precede the satellite's joined detach reply. A
            // fresh proxy must never mistake them for its new attach prefix.
            return Ok(());
        }
        if self.enforce_bootstrap_flow {
            self.enforce_stream_frame_flow(id, &frame)?;
        }
        let retagged = self.retag_stream_frame(frame, id);
        self.fan_out(id, &retagged);
        Ok(())
    }

    /// The bootstrap-flow gate each stream frame kind must clear before it is
    /// forwarded. `EVENT` carries no flow state and clears trivially.
    fn enforce_stream_frame_flow(&mut self, id: u32, frame: &FrameKind) -> Result<(), String> {
        match frame {
            FrameKind::TerminalOutput {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "TERMINAL_OUTPUT"),
            FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                profile,
                ..
            } => self.begin_bootstrap_flow(id, *stream_id, *bootstrap_id, *profile),
            FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
                ..
            } => self.accept_bootstrap_chunk(
                id,
                *stream_id,
                *bootstrap_id,
                *chunk_seq,
                payload.len(),
            ),
            FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            } => self.finish_bootstrap_flow(id, *stream_id, *bootstrap_id),
            FrameKind::HistoryPage {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "HISTORY_PAGE"),
            FrameKind::BootstrapTombstone {
                stream_id,
                bootstrap_id,
                ..
            } => {
                self.validate_known_identity(id, *stream_id, *bootstrap_id, "BOOTSTRAP_TOMBSTONE")?;
                self.retire_bootstrap_flow(id);
                Ok(())
            }
            FrameKind::HistoryTombstone {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "HISTORY_TOMBSTONE"),
            FrameKind::HistoryRejected {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "HISTORY_REJECTED"),
            _ => Ok(()),
        }
    }

    /// Re-tag one stream frame's terminal scope `Local { id }` ->
    /// `Satellite { host, id }`. Every other field is forwarded verbatim
    /// (ADR-0007: opaque relay), so the scope is rewritten in place rather
    /// than the frame rebuilt field by field.
    fn retag_stream_frame(&self, mut frame: FrameKind, id: u32) -> FrameKind {
        let scope = TerminalId::satellite(self.host.clone(), id);
        match &mut frame {
            FrameKind::Event { terminal, .. } => *terminal = Some(scope),
            FrameKind::TerminalOutput { terminal_id, .. }
            | FrameKind::BootstrapBegin { terminal_id, .. }
            | FrameKind::BootstrapChunk { terminal_id, .. }
            | FrameKind::BootstrapReady { terminal_id, .. }
            | FrameKind::HistoryPage { terminal_id, .. }
            | FrameKind::BootstrapTombstone { terminal_id, .. }
            | FrameKind::HistoryTombstone { terminal_id, .. }
            | FrameKind::HistoryRejected { terminal_id, .. } => *terminal_id = scope,
            _ => {}
        }
        frame
    }

    /// Deliver `TERMINAL_CLOSED`, then reap everything the satellite terminal
    /// owned on this link.
    ///
    /// `reason` is the satellite's (ADR-0104 §4): the hub retags the id and
    /// forwards the fact unchanged. A hub that substituted its own reason
    /// would tell a consumer a satellite-side cascade was a plain exit.
    fn relay_terminal_closed(
        &mut self,
        terminal_id: &TerminalId,
        exit_status: Option<i32>,
        reason: phux_protocol::wire::frame::CloseReason,
    ) {
        let Some(id) = self.retag_inbound(Some(terminal_id)) else {
            return;
        };
        // Best-effort delivery bypassing the snapshot gate
        // (phux-v45.14 sub-finding a): the subscriptions are
        // reaped on the next line, so a subscriber still awaiting
        // its first snapshot must still learn the terminal closed
        // rather than be silently dropped.
        self.fan_out_ungated(
            id,
            &FrameKind::TerminalClosed {
                terminal_id: TerminalId::satellite(self.host.clone(), id),
                exit_status,
                reason,
            },
        );
        // The satellite terminal is gone; its proxy
        // subscriptions go with it.
        self.subscribers.remove(&id);
        self.recalculate_retained_totals();
        self.retire_bootstrap_flow(id);
        self.explicit_content.remove(&id);
        self.legacy_events.remove(&id);
    }

    /// Deliver one `BELL` side-channel notification.
    fn relay_bell(&mut self, terminal_id: &TerminalId) {
        let Some(id) = self.retag_inbound(Some(terminal_id)) else {
            return;
        };
        // Best-effort delivery bypassing the snapshot gate
        // (phux-v45.15): a BELL is an ephemeral notification the
        // `TERMINAL_SNAPSHOT` does not capture, so gating it behind
        // an `AwaitingFirst` subscriber's snapshot would drop it
        // permanently — unlike a `TERMINAL_OUTPUT` delta, which the
        // snapshot supersedes (freshest full grid wins), so gating
        // content is safe but gating a bell loses it. Ordering
        // against the snapshot does not matter for a side-channel
        // notification, the same rationale as `TERMINAL_CLOSED`.
        self.fan_out_ungated(
            id,
            &FrameKind::Bell {
                terminal_id: TerminalId::satellite(self.host.clone(), id),
            },
        );
    }

    /// Fail every in-flight command and notify every subscribed consumer,
    /// then clear the registries. Called exactly once per session, on
    /// disconnect or hub shutdown.
    pub(crate) fn teardown(&mut self, why: &str) {
        for (_, pending) in self.pending.drain() {
            let _ = pending.reply.send(CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: format!("satellite {} is unreachable: {why}", self.host),
            });
        }
        for (_, reply) in self.pending_spawns.drain() {
            let _ = reply.send(SpawnResult::Err(SpawnError::SatelliteUnreachable(format!(
                "satellite {} is unreachable: {why}",
                self.host
            ))));
        }
        // One typed ERROR per consumer (not per subscription): the frame
        // names the host, and every terminal of that host is gone at once.
        let mut notified: Vec<ClientId> = Vec::new();
        let error = unreachable_error(&self.host, why);
        for subs in self.subscribers.values() {
            for sub in subs {
                if notified.contains(&sub.client) {
                    continue;
                }
                notified.push(sub.client);
                let _ = sub.out_tx.try_send(Outbound::Frame(error.clone()));
            }
        }
        if !notified.is_empty() {
            debug!(
                satellite = %self.host,
                consumers = notified.len(),
                why,
                "notified proxy subscribers of satellite teardown"
            );
        }
        self.subscribers.clear();
        self.bootstrap_flows.clear();
        self.explicit_content.clear();
        self.legacy_events.clear();
        self.pending_detaches.clear();
        self.retained_bytes = 0;
        self.retained_frames = 0;
        self.inflight_generation_bytes = 0;
        self.inflight_generation_frames = 0;
    }

    /// Drop pending entries whose consumer stopped waiting (the
    /// [`RelayHandle::command`] deadline elapsed, the consumer
    /// disconnected, or the relay was detached from the start). Returns
    /// how many entries were pruned.
    ///
    /// Called from the link supervisor's keepalive tick: without it, a
    /// satellite that reads relayed commands but never answers would grow
    /// the pending map without bound (only [`RELAY_MAILBOX`] entries drain
    /// per mailbox refill, and nothing else removes them).
    pub(crate) fn prune_abandoned(&mut self) -> usize {
        let before = self.pending.len() + self.pending_spawns.len();
        self.pending.retain(|_, pending| !pending.reply.is_closed());
        self.pending_spawns.retain(|_, reply| !reply.is_closed());
        let pruned = before - self.pending.len() - self.pending_spawns.len();
        if pruned > 0 {
            debug!(
                satellite = %self.host,
                pruned,
                remaining = self.pending.len() + self.pending_spawns.len(),
                "pruned relayed commands whose consumer stopped waiting"
            );
        }
        pruned
    }

    /// A peer that remains alive but never acknowledges teardown must not
    /// retain barriers indefinitely or strand a new proxy behind one.
    /// Unlike an abandoned read-only command, a missing teardown receipt leaves
    /// upstream generation ownership uncertain. Reset the shared link rather
    /// than releasing stale content into any subsequent bootstrap.
    pub(crate) fn check_detach_deadlines(&self) -> Result<(), String> {
        let now = tokio::time::Instant::now();
        if self
            .pending_detaches
            .values()
            .any(|pending| pending.deadline <= now)
        {
            return Err(format!(
                "satellite {} did not acknowledge upstream detach within {}s",
                self.host,
                RELAY_COMMAND_TIMEOUT.as_secs()
            ));
        }
        Ok(())
    }

    /// Resolve a link-side `request_id` back to its waiting consumer.
    ///
    /// An error reply rolls back the command's atomic subscription rider
    /// when this command was the one that created it (phux-v45.11
    /// finding 3): the satellite refused, so nothing will ever stream for
    /// that registration and keeping it would fan future frames (from a
    /// later, unrelated subscriber's stream) to a consumer that was told
    /// its subscribe failed.
    fn resolve_pending(&mut self, request_id: u32, result: CommandResult) {
        match self.pending.remove(&request_id) {
            Some(pending) => {
                if matches!(result, CommandResult::Error { .. })
                    && let Some((terminal, client, effect)) = pending.subscription
                {
                    self.roll_back_subscription(terminal, client, effect);
                }
                // A dropped receiver (detached relay) is fine.
                let _ = pending.reply.send(result);
            }
            None => {
                debug!(
                    satellite = %self.host,
                    request_id,
                    "satellite reply with no pending command; dropping"
                );
            }
        }
    }

    /// Undo the registration effect a failed subscribing command had
    /// (phux-v45.11 finding 3, phux-v45.15). The satellite refused, so this
    /// registration's own stream and snapshot never come.
    fn roll_back_subscription(&mut self, terminal: u32, client: ClientId, effect: Registration) {
        match effect {
            // The command created the subscriber: remove it, or a later
            // unrelated subscriber's stream would fan out to a consumer told
            // its subscribe failed.
            Registration::New => {
                if let Some(subs) = self.subscribers.get_mut(&terminal) {
                    subs.retain(|s| s.client != client);
                    if subs.is_empty() {
                        self.subscribers.remove(&terminal);
                    }
                    debug!(
                        satellite = %self.host,
                        terminal,
                        ?client,
                        "satellite refused the subscribing command; proxy registration rolled back"
                    );
                }
            }
            // The command upgraded an already-`Open` stream to an attach and
            // re-gated it to `AwaitingFirst`; the attach's snapshot never
            // comes, so restore the gate to `Open` or the pre-existing stream
            // is stranded (phux-v45.15). Do so only if this command's gate is
            // still present: a snapshot retained while the reply was in
            // flight must stay ahead of later deltas (phux-v45.16).
            Registration::Regated => {
                if let Some(sub) = self
                    .subscribers
                    .get_mut(&terminal)
                    .and_then(|subs| subs.iter_mut().find(|s| s.client == client))
                    && matches!(sub.gate, SnapshotGate::AwaitingFirst)
                {
                    sub.gate = SnapshotGate::Open;
                    debug!(
                        satellite = %self.host,
                        terminal,
                        ?client,
                        "satellite refused the upgrade attach; re-gated stream restored to Open"
                    );
                }
            }
            // A pre-existing registration this command did not touch: nothing
            // to undo.
            Registration::Idempotent => {}
        }
        self.recalculate_retained_totals();
    }

    /// Resolve a link-side spawn `request_id` back to its waiting
    /// consumer, re-tagging a successful result's freshly allocated id
    /// `Local { id }` -> `Satellite { host, id }` (phux-v45.6). A
    /// `Satellite`-tagged id in the satellite's own reply is out of the
    /// hub-and-spoke topology and resolves as a `SpawnFailed` error
    /// rather than being chained onward.
    fn resolve_pending_spawn(&mut self, request_id: u32, result: SpawnResult) {
        let Some(reply) = self.pending_spawns.remove(&request_id) else {
            debug!(
                satellite = %self.host,
                request_id,
                "satellite spawn reply with no pending spawn; dropping"
            );
            return;
        };
        let retagged = match result {
            SpawnResult::Ok(TerminalId::Local { id }) => {
                SpawnResult::Ok(TerminalId::satellite(self.host.clone(), id))
            }
            SpawnResult::Ok(TerminalId::Satellite { .. }) => {
                warn!(
                    satellite = %self.host,
                    "satellite answered a spawn with a Satellite-tagged id; hub-and-spoke does not chain"
                );
                SpawnResult::Err(SpawnError::SpawnFailed(
                    "satellite returned a chained satellite id".to_owned(),
                ))
            }
            err @ SpawnResult::Err(_) => err,
            // `SpawnResult` is `#[non_exhaustive]`: a future variant a
            // newer satellite sends passes through untouched (it carries
            // no terminal id to re-tag).
            other => other,
        };
        // A dropped receiver (consumer timed out / disconnected) is fine.
        let _ = reply.send(retagged);
    }

    /// The satellite-local id of an inbound frame's terminal scope, or
    /// `None` when the frame is unscoped or (out of ADR-0007 topology)
    /// already satellite-tagged — satellites do not chain.
    fn retag_inbound(&self, terminal: Option<&TerminalId>) -> Option<u32> {
        match terminal {
            Some(TerminalId::Local { id }) => Some(*id),
            Some(TerminalId::Satellite { .. }) => {
                warn!(
                    satellite = %self.host,
                    "satellite forwarded a Satellite-tagged id; hub-and-spoke does not chain — dropping"
                );
                None
            }
            None => None,
        }
    }
    const fn expected_stream_profile(&self) -> Option<BootstrapStreamProfile> {
        match self.bootstrap_profile {
            BootstrapProfile::NativeState { codec, .. } => {
                Some(BootstrapStreamProfile::NativeState { codec })
            }
            BootstrapProfile::SynthesizedVtRaw => Some(BootstrapStreamProfile::SynthesizedVtRaw),
            BootstrapProfile::SynthesizedVtStateSync => {
                Some(BootstrapStreamProfile::SynthesizedVtStateSync)
            }
            _ => None,
        }
    }

    fn begin_bootstrap_flow(
        &mut self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
    ) -> Result<(), String> {
        if Some(profile) != self.expected_stream_profile() {
            return Err(format!(
                "satellite {} sent BOOTSTRAP_BEGIN profile {profile:?}, negotiated {:?}",
                self.host, self.bootstrap_profile
            ));
        }
        if self.bootstrap_flows.contains_key(&terminal) {
            return Err(format!(
                "satellite {} sent overlapping BOOTSTRAP_BEGIN for terminal {terminal}",
                self.host
            ));
        }
        if self.inflight_generation_frames >= MAX_RELAY_GENERATION_FRAMES * 2 {
            return Err(format!(
                "satellite {} exceeded the connection-wide in-flight bootstrap frame budget",
                self.host
            ));
        }
        self.inflight_generation_frames += 1;
        self.bootstrap_flows.insert(
            terminal,
            RelayBootstrapFlow {
                stream_id,
                bootstrap_id,
                profile,
                next_chunk_seq: 0,
                generation_bytes: 0,
                generation_frames: 1,
                ready: false,
            },
        );
        Ok(())
    }

    fn accept_bootstrap_chunk(
        &mut self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload_len: usize,
    ) -> Result<(), String> {
        let payload_len = u64::try_from(payload_len)
            .map_err(|_| format!("satellite {} sent an oversized bootstrap chunk", self.host))?;
        let Some(flow) = self.bootstrap_flows.get_mut(&terminal) else {
            return Err(format!(
                "satellite {} sent BOOTSTRAP_CHUNK before BEGIN for terminal {terminal}",
                self.host
            ));
        };
        ensure_chunk_identity(&self.host, terminal, flow, stream_id, bootstrap_id)?;
        ensure_chunk_sequence(&self.host, flow, chunk_seq)?;
        let next_frames = charge_frame(
            &self.host,
            "per-generation bootstrap frame budget",
            flow.generation_frames,
            MAX_RELAY_GENERATION_FRAMES,
        )?;
        let next_bytes = charge_bytes(
            &self.host,
            "per-generation bootstrap byte budget",
            flow.generation_bytes,
            payload_len,
            MAX_RELAY_GENERATION_BYTES,
        )?;
        let connection_frames = charge_frame(
            &self.host,
            "connection-wide in-flight bootstrap frame budget",
            self.inflight_generation_frames,
            MAX_RELAY_GENERATION_FRAMES * 2,
        )?;
        let connection_bytes = charge_bytes(
            &self.host,
            "connection-wide in-flight bootstrap byte budget",
            self.inflight_generation_bytes,
            payload_len,
            MAX_RELAY_GENERATION_BYTES * 2,
        )?;
        flow.next_chunk_seq = flow.next_chunk_seq.checked_add(1).ok_or_else(|| {
            format!(
                "satellite {} overflowed the bootstrap chunk sequence",
                self.host
            )
        })?;
        flow.generation_frames = next_frames;
        flow.generation_bytes = next_bytes;
        self.inflight_generation_frames = connection_frames;
        self.inflight_generation_bytes = connection_bytes;
        Ok(())
    }

    fn finish_bootstrap_flow(
        &mut self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) -> Result<(), String> {
        let Some(flow) = self.bootstrap_flows.get_mut(&terminal) else {
            return Err(format!(
                "satellite {} sent BOOTSTRAP_READY before BEGIN for terminal {terminal}",
                self.host
            ));
        };
        if flow.ready || flow.stream_id != stream_id || flow.bootstrap_id != bootstrap_id {
            return Err(format!(
                "satellite {} changed or reused bootstrap identity at READY for terminal {terminal}",
                self.host
            ));
        }
        if flow.generation_frames >= MAX_RELAY_GENERATION_FRAMES {
            return Err(format!(
                "satellite {} exceeded the per-generation bootstrap frame budget",
                self.host
            ));
        }
        flow.generation_frames += 1;
        flow.ready = true;
        self.inflight_generation_bytes = self
            .inflight_generation_bytes
            .saturating_sub(flow.generation_bytes);
        self.inflight_generation_frames = self
            .inflight_generation_frames
            .saturating_sub(flow.generation_frames.saturating_sub(1));
        Ok(())
    }
    fn validate_known_identity(
        &self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        frame_name: &str,
    ) -> Result<(), String> {
        let Some(flow) = self.bootstrap_flows.get(&terminal) else {
            return Err(format!(
                "satellite {} sent {frame_name} without a selected bootstrap generation for terminal {terminal}",
                self.host
            ));
        };
        if flow.stream_id != stream_id
            || flow.bootstrap_id != bootstrap_id
            || Some(flow.profile) != self.expected_stream_profile()
        {
            return Err(format!(
                "satellite {} sent {frame_name} for a non-active bootstrap identity on terminal {terminal}",
                self.host
            ));
        }
        Ok(())
    }

    fn validate_ready_identity(
        &self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        frame_name: &str,
    ) -> Result<(), String> {
        self.validate_known_identity(terminal, stream_id, bootstrap_id, frame_name)?;
        if !self.bootstrap_flows[&terminal].ready {
            return Err(format!(
                "satellite {} sent {frame_name} before BOOTSTRAP_READY for terminal {terminal}",
                self.host
            ));
        }
        Ok(())
    }

    fn retire_bootstrap_flow(&mut self, terminal: u32) {
        if let Some(flow) = self.bootstrap_flows.remove(&terminal)
            && !flow.ready
        {
            self.inflight_generation_bytes = self
                .inflight_generation_bytes
                .saturating_sub(flow.generation_bytes);
            self.inflight_generation_frames = self
                .inflight_generation_frames
                .saturating_sub(flow.generation_frames);
        }
    }

    /// Push `frame` to every proxy subscriber of satellite-local terminal
    /// `id`. `try_send` per consumer: a slow consumer drops its copy, the
    /// link and its siblings keep flowing.
    ///
    /// A `TERMINAL_SNAPSHOT` is the ordering anchor (L1 §9.1): a subscriber
    /// receives content deltas only once its own snapshot has landed, gated
    /// by [`SnapshotGate`]. Two cases hold the guarantee across the two-hop
    /// attach. First, a second consumer attaching to a terminal already
    /// streaming to another consumer starts [`SnapshotGate::AwaitingFirst`]
    /// (phux-v45.14): the ongoing stream's `TERMINAL_OUTPUT` is suppressed
    /// for it until its own attach snapshot fans out. Second, if a
    /// consumer's briefly-full mailbox refuses that snapshot it is
    /// **retained** (phux-v45.12, [`SnapshotGate::Retained`]) and retried —
    /// here on the next delta, and on the keepalive tick — before any delta
    /// may ride, with a newer snapshot (a satellite resync) replacing it.
    /// This mirrors the local attach's snapshot gate without blocking the
    /// link: a sustained-saturation consumer may still lag on *content* (the
    /// pre-existing slow-consumer condition) but never sees a delta before a
    /// snapshot. `TERMINAL_CLOSED` and `BELL` are the exceptions
    /// ([`Self::fan_out_ungated`]): snapshot-independent lifecycle / notice
    /// frames the snapshot does not capture, best-effort delivered past the
    /// gate rather than dropped.
    fn fan_out(&mut self, id: u32, frame: &FrameKind) {
        let host = &self.host;
        let Some(subs) = self.subscribers.get_mut(&id) else {
            trace!(satellite = %host, terminal = id, "inbound stream frame with no proxy subscribers");
            return;
        };
        let is_begin = matches!(frame, FrameKind::BootstrapBegin { .. });
        let is_chunk = matches!(frame, FrameKind::BootstrapChunk { .. });
        let is_ready = matches!(frame, FrameKind::BootstrapReady { .. });
        let replace_legacy_begin = is_begin && !self.enforce_bootstrap_flow;
        let is_tombstone = matches!(frame, FrameKind::BootstrapTombstone { .. });
        let mut retained_bytes = self.retained_bytes;
        let mut retained_frames = self.retained_frames;
        subs.retain_mut(|sub| {
            if replace_legacy_begin {
                sub.gate = SnapshotGate::AwaitingFirst;
            }
            if is_begin || is_chunk || is_ready || is_tombstone {
                return Self::push_bootstrap_frame(
                    sub,
                    frame.clone(),
                    is_ready,
                    &mut retained_bytes,
                    &mut retained_frames,
                );
            }
            let (may_send, alive) = Self::flush_pending_snapshot(sub);
            if !alive {
                return false;
            }
            if !may_send {
                return true;
            }
            matches!(sub.out_tx.try_send(Outbound::Frame(frame.clone())), Ok(()))
        });
        self.retained_bytes = retained_bytes;
        self.retained_frames = retained_frames;
        if subs.is_empty() {
            self.subscribers.remove(&id);
        }
        self.recalculate_retained_totals();
    }

    fn push_bootstrap_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        open_after: bool,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) -> bool {
        let frame_bytes = Self::retained_frame_bytes(&frame);
        let gate = std::mem::replace(&mut sub.gate, SnapshotGate::AwaitingFirst);
        match gate {
            SnapshotGate::Retained {
                mut frames,
                retained_bytes,
                open_after: prior_open_after,
            } => {
                let next_bytes = retained_bytes.saturating_add(frame_bytes);
                let next_frames = frames.len().saturating_add(1);
                if next_bytes > MAX_RELAY_SUBSCRIBER_RETAINED_BYTES
                    || next_frames > MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES
                    || connection_bytes.saturating_add(frame_bytes)
                        > MAX_RELAY_CONNECTION_RETAINED_BYTES
                    || connection_frames.saturating_add(1) > MAX_RELAY_CONNECTION_RETAINED_FRAMES
                {
                    *connection_bytes = connection_bytes.saturating_sub(retained_bytes);
                    *connection_frames = connection_frames.saturating_sub(frames.len());
                    warn!(
                        client = ?sub.client,
                        "reaping saturated relay subscriber whose retained bootstrap exceeded bounds"
                    );
                    sub.gate = SnapshotGate::Open;
                    return false;
                }
                frames.push_back(frame);
                *connection_bytes += frame_bytes;
                *connection_frames += 1;
                sub.gate = SnapshotGate::Retained {
                    frames,
                    retained_bytes: next_bytes,
                    open_after: prior_open_after || open_after,
                };
                true
            }
            SnapshotGate::AwaitingFirst | SnapshotGate::Open => {
                match sub.out_tx.try_send(Outbound::Frame(frame.clone())) {
                    Ok(()) => {
                        sub.gate = if open_after {
                            SnapshotGate::Open
                        } else {
                            SnapshotGate::AwaitingFirst
                        };
                        true
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        if frame_bytes > MAX_RELAY_SUBSCRIBER_RETAINED_BYTES
                            || connection_bytes.saturating_add(frame_bytes)
                                > MAX_RELAY_CONNECTION_RETAINED_BYTES
                            || connection_frames.saturating_add(1)
                                > MAX_RELAY_CONNECTION_RETAINED_FRAMES
                        {
                            sub.gate = SnapshotGate::Open;
                            return false;
                        }
                        *connection_bytes += frame_bytes;
                        *connection_frames += 1;
                        sub.gate = SnapshotGate::Retained {
                            frames: VecDeque::from([frame]),
                            retained_bytes: frame_bytes,
                            open_after,
                        };
                        true
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        sub.gate = SnapshotGate::Open;
                        false
                    }
                }
            }
        }
    }

    fn retained_frame_bytes(frame: &FrameKind) -> usize {
        let payload = match frame {
            FrameKind::BootstrapChunk { payload, .. } => payload.len(),
            FrameKind::BootstrapReady { history_cursor, .. } => {
                history_cursor.as_ref().map_or(0, bytes::Bytes::len)
            }
            _ => 0,
        };
        payload.saturating_add(RETAINED_FRAME_OVERHEAD)
    }

    /// Best-effort deliver a snapshot-independent frame to every proxy
    /// subscriber, **bypassing** the snapshot gate. Two return-leg frames take
    /// this path: `TERMINAL_CLOSED` (phux-v45.14 sub-finding a) and `BELL`
    /// (phux-v45.15). Neither is content the `TERMINAL_SNAPSHOT` captures, so
    /// gating them behind an `AwaitingFirst` subscriber's not-yet-delivered
    /// snapshot would drop them permanently — a close would tear the consumer
    /// down before it learned its terminal is gone, and a bell notification
    /// would simply vanish. A content `TERMINAL_OUTPUT` delta, by contrast,
    /// the snapshot supersedes (freshest full grid wins), so gating it is
    /// safe; these are not. Ordering against the snapshot is irrelevant for a
    /// lifecycle signal or a side-channel notification. `try_send`,
    /// fire-and-forget: refusal reaps the saturated or closed subscriber so
    /// a permanently unread mailbox cannot retain relay state.
    fn fan_out_ungated(&mut self, id: u32, frame: &FrameKind) {
        let Some(subs) = self.subscribers.get_mut(&id) else {
            trace!(satellite = %self.host, terminal = id, "ungated frame with no proxy subscribers");
            return;
        };
        subs.retain(|sub| matches!(sub.out_tx.try_send(Outbound::Frame(frame.clone())), Ok(())));
        if subs.is_empty() {
            self.subscribers.remove(&id);
        }
        self.recalculate_retained_totals();
    }

    /// Retry one subscriber's retained bootstrap prefix (phux-v45.12).
    /// Returns `(may_send_delta, subscriber_alive)`: a full mailbox remains
    /// retained, an `AwaitingFirst` gate still suppresses deltas, and a closed
    /// mailbox is reported dead so callers reap it immediately.
    fn flush_pending_snapshot(sub: &mut ProxySubscriber) -> (bool, bool) {
        let gate = std::mem::replace(&mut sub.gate, SnapshotGate::AwaitingFirst);
        let SnapshotGate::Retained {
            mut frames,
            mut retained_bytes,
            open_after,
        } = gate
        else {
            let open = matches!(gate, SnapshotGate::Open);
            sub.gate = gate;
            return (open, true);
        };
        while let Some(frame) = frames.front() {
            match sub.out_tx.try_send(Outbound::Frame(frame.clone())) {
                Ok(()) => {
                    retained_bytes =
                        retained_bytes.saturating_sub(Self::retained_frame_bytes(frame));
                    frames.pop_front();
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    sub.gate = SnapshotGate::Retained {
                        frames,
                        retained_bytes,
                        open_after,
                    };
                    return (false, true);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    sub.gate = SnapshotGate::Open;
                    return (false, false);
                }
            }
        }
        sub.gate = if open_after {
            SnapshotGate::Open
        } else {
            SnapshotGate::AwaitingFirst
        };
        (open_after, true)
    }

    /// Retry every subscriber's retained attach snapshot (phux-v45.12).
    /// Driven from the link supervisor's keepalive tick so a consumer whose
    /// mailbox was briefly full at attach still converges even if no further
    /// return-leg frame arrives for its terminal to trigger the inline retry
    /// in [`Self::fan_out`].
    pub(crate) fn flush_pending_snapshots(&mut self) {
        for subs in self.subscribers.values_mut() {
            subs.retain_mut(|sub| {
                if matches!(sub.gate, SnapshotGate::Retained { .. }) {
                    Self::flush_pending_snapshot(sub).1
                } else {
                    true
                }
            });
        }
        self.subscribers.retain(|_, subs| !subs.is_empty());
        self.recalculate_retained_totals();
    }

    fn recalculate_retained_totals(&mut self) {
        let mut bytes = 0usize;
        let mut frames = 0usize;
        for sub in self.subscribers.values().flatten() {
            if let SnapshotGate::Retained {
                frames: queued,
                retained_bytes,
                ..
            } = &sub.gate
            {
                bytes = bytes.saturating_add(*retained_bytes);
                frames = frames.saturating_add(queued.len());
            }
        }
        self.retained_bytes = bytes;
        self.retained_frames = frames;
    }

    /// Allocate the next link-side request id, skipping ids still pending
    /// in any reply map (u32 wrap-around safety, not a practical
    /// collision).
    fn allocate_request_id(&mut self) -> u32 {
        loop {
            let id = self.next_request_id;
            self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
            if !self.pending.contains_key(&id)
                && !self.pending_spawns.contains_key(&id)
                && !self.pending_detaches.contains_key(&id)
            {
                return id;
            }
        }
    }

    fn encode(&mut self, frame: &FrameKind) -> Vec<u8> {
        self.encode_buf.clear();
        frame.encode(&mut self.encode_buf);
        self.encode_buf.to_vec()
    }
}

/// Split a satellite-tagged wire id into its host and satellite-local id.
pub(crate) fn satellite_route(terminal_id: &TerminalId) -> Option<(SatelliteHost, u32)> {
    match terminal_id {
        TerminalId::Satellite { host, id } => Some((host.clone(), *id)),
        TerminalId::Local { .. } => None,
    }
}

/// If `command` targets a single satellite-owned terminal, produce the
/// owning host and the command rewritten to the satellite's `Local` id
/// space (ADR-0007 outbound leg). `None` for local targets, unscoped
/// commands (`GET_STATE`, `UPGRADE` — hub-local by design), and
/// `KILL_TERMINALS` (a mixed batch, partitioned by its own handler).
#[allow(
    clippy::too_many_lines,
    reason = "one mechanical rewrite arm per per-terminal Command variant; splitting hides the catalog"
)]
pub(crate) fn route_to_satellite(command: &Command) -> Option<(SatelliteHost, Command)> {
    match command {
        Command::AttachTerminal { terminal_id } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::AttachTerminal {
                    terminal_id: TerminalId::local(id),
                },
            ))
        }
        Command::DetachTerminal { terminal_id } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::DetachTerminal {
                    terminal_id: TerminalId::local(id),
                },
            ))
        }
        Command::KillTerminal { terminal_id } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::KillTerminal {
                    terminal_id: TerminalId::local(id),
                },
            ))
        }
        Command::GetScreen {
            terminal_id,
            request_scrollback,
            cells,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::GetScreen {
                    terminal_id: TerminalId::local(id),
                    request_scrollback: *request_scrollback,
                    cells: *cells,
                },
            ))
        }
        Command::RouteInput { terminal_id, event } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::RouteInput {
                    terminal_id: TerminalId::local(id),
                    event: event.clone(),
                },
            ))
        }
        Command::GetTerminalState {
            terminal_id,
            include_scrollback,
            max_scrollback_lines,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::GetTerminalState {
                    terminal_id: TerminalId::local(id),
                    include_scrollback: *include_scrollback,
                    max_scrollback_lines: *max_scrollback_lines,
                },
            ))
        }
        Command::SubscribeTerminalEvents {
            terminal_id,
            event_types,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::SubscribeTerminalEvents {
                    terminal_id: TerminalId::local(id),
                    event_types: event_types.clone(),
                },
            ))
        }
        Command::AcquireInput {
            terminal_id,
            mode,
            ttl_ms,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::AcquireInput {
                    terminal_id: TerminalId::local(id),
                    mode: *mode,
                    ttl_ms: *ttl_ms,
                },
            ))
        }
        Command::ReleaseInput { terminal_id } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::ReleaseInput {
                    terminal_id: TerminalId::local(id),
                },
            ))
        }
        Command::SignalTerminal {
            terminal_id,
            signal,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::SignalTerminal {
                    terminal_id: TerminalId::local(id),
                    signal: *signal,
                },
            ))
        }
        Command::PutFile {
            upload_id,
            terminal_id,
            extension,
            offset,
            data,
            final_chunk,
            sha256,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::PutFile {
                    upload_id: *upload_id,
                    terminal_id: TerminalId::local(id),
                    extension: extension.clone(),
                    offset: *offset,
                    data: data.clone(),
                    final_chunk: *final_chunk,
                    sha256: *sha256,
                },
            ))
        }
        Command::ReportAsked {
            terminal_id,
            id: asked_id,
            question,
            suggestions,
            elapsed_seconds,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::ReportAsked {
                    terminal_id: TerminalId::local(id),
                    id: asked_id.clone(),
                    question: question.clone(),
                    suggestions: suggestions.clone(),
                    elapsed_seconds: *elapsed_seconds,
                },
            ))
        }
        Command::ReportAgentState { terminal_id, state } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::ReportAgentState {
                    terminal_id: TerminalId::local(id),
                    state: *state,
                },
            ))
        }
        // GET_STATE / UPGRADE are hub-local; KILL_TERMINALS partitions its
        // mixed batch in `handle_kill_terminals`; forward-compat commands
        // this hub does not know cannot be routed (their terminal scope is
        // unreadable) and fall through to the local INVALID_COMMAND path.
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use phux_protocol::wire::frame::{AgentEvent, CommandValue, StateScope};

    use super::*;

    fn host() -> SatelliteHost {
        SatelliteHost::new("devbox")
    }

    fn decode(bytes: &[u8]) -> FrameKind {
        FrameKind::decode(bytes).expect("frame").0
    }

    fn encode(frame: &FrameKind) -> Vec<u8> {
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        buf.to_vec()
    }

    /// Register a proxy subscription through the atomic Subscribe request
    /// (the `SUBSCRIBE_EVENTS` shape) and assert the paired forward frame
    /// was produced. Registers at the baseline issue-order token 1;
    /// [`subscribe_at`] controls the token for reorder tests.
    fn subscribe(
        session: &mut RelaySession,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) {
        subscribe_at(session, terminal, client, 1, out_tx);
    }

    /// [`subscribe`] with an explicit issue-order token, for exercising
    /// the detach/reattach reorder guard (phux-v45.7).
    fn subscribe_at(
        session: &mut RelaySession,
        terminal: u32,
        client: ClientId,
        seq: u64,
        out_tx: mpsc::Sender<Outbound>,
    ) {
        let wire = session.handle_request(RelayRequest::Subscribe {
            subscription: ProxySubscription {
                terminal,
                client,
                out_tx,
                seq,
                // The SUBSCRIBE_EVENTS shape: no return-leg snapshot, so the
                // subscriber opens ungated.
                awaits_snapshot: false,
                bootstrap_profile: None,
                bootstrap_limits: None,
            },
            forward: FrameKind::SubscribeEvents {
                terminal: Some(TerminalId::local(terminal)),
            },
        });
        assert!(
            !wire.is_empty(),
            "atomic subscribe must produce the forward frame"
        );
    }

    /// Register an `ATTACH_TERMINAL` proxy subscription (the snapshot-bearing
    /// content-stream shape, phux-v45.14): the subscriber starts gated and
    /// its deltas are suppressed until its own return-leg `TERMINAL_SNAPSHOT`
    /// lands. The command reply receiver is dropped — the registration is
    /// applied synchronously in `handle_request`, which is all these
    /// ordering tests exercise.
    fn attach(
        session: &mut RelaySession,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) {
        let selected_profile = session.bootstrap_profile;
        let selected_limits = session.bootstrap_limits;
        let (reply, _rx) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::AttachTerminal {
                terminal_id: TerminalId::local(terminal),
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal,
                client,
                out_tx,
                seq: 1,
                awaits_snapshot: true,
                bootstrap_profile: Some(selected_profile),
                bootstrap_limits: Some(selected_limits),
            }),
        });
        assert!(!wire.is_empty(), "attach must produce the COMMAND frame");
    }

    // --- outbound command rewrite ---------------------------------------

    #[test]
    fn route_to_satellite_rewrites_terminal_ids_to_local() {
        let command = Command::GetScreen {
            terminal_id: TerminalId::satellite("devbox", 7),
            request_scrollback: Some(10),
            cells: true,
        };
        let (routed_host, rewritten) = route_to_satellite(&command).expect("satellite target");
        assert_eq!(routed_host, host());
        assert_eq!(
            rewritten,
            Command::GetScreen {
                terminal_id: TerminalId::local(7),
                request_scrollback: Some(10),
                cells: true,
            }
        );
    }

    #[test]
    fn route_to_satellite_ignores_local_and_unscoped_commands() {
        assert!(
            route_to_satellite(&Command::GetScreen {
                terminal_id: TerminalId::local(7),
                request_scrollback: None,
                cells: false,
            })
            .is_none()
        );
        assert!(
            route_to_satellite(&Command::GetState {
                scope: StateScope::Server,
            })
            .is_none()
        );
        assert!(route_to_satellite(&Command::Upgrade).is_none());
        assert!(
            route_to_satellite(&Command::ApplyInput {
                operation_id: phux_protocol::InputOperationId::new([1; 16]).expect("id"),
                terminal_id: TerminalId::satellite("devbox", 7),
                events: vec![],
            })
            .is_none(),
            "APPLY_INPUT is local-only and must never touch a satellite link"
        );
        // Mixed batches partition in handle_kill_terminals, not here.
        assert!(
            route_to_satellite(&Command::KillTerminals {
                ids: vec![TerminalId::satellite("devbox", 1)],
            })
            .is_none()
        );
    }

    #[test]
    fn route_to_satellite_covers_every_per_terminal_command() {
        let sat = TerminalId::satellite("devbox", 3);
        let commands = [
            Command::AttachTerminal {
                terminal_id: sat.clone(),
            },
            Command::DetachTerminal {
                terminal_id: sat.clone(),
            },
            Command::KillTerminal {
                terminal_id: sat.clone(),
            },
            Command::GetTerminalState {
                terminal_id: sat.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
            Command::SubscribeTerminalEvents {
                terminal_id: sat.clone(),
                event_types: vec![],
            },
            Command::AcquireInput {
                terminal_id: sat.clone(),
                mode: phux_protocol::wire::frame::InputMode::Cooperative,
                ttl_ms: 0,
            },
            Command::ReleaseInput {
                terminal_id: sat.clone(),
            },
            Command::SignalTerminal {
                terminal_id: sat.clone(),
                signal: phux_protocol::wire::frame::TerminalSignal::Interrupt,
            },
            Command::ReportAsked {
                terminal_id: sat.clone(),
                id: "q".to_owned(),
                question: "?".to_owned(),
                suggestions: vec![],
                elapsed_seconds: None,
            },
            Command::ReportAgentState {
                terminal_id: sat,
                state: phux_protocol::wire::frame::ReportedAgentState::Done,
            },
        ];
        for command in commands {
            let (routed_host, rewritten) =
                route_to_satellite(&command).expect("per-terminal command routes");
            assert_eq!(routed_host, host());
            assert!(
                route_to_satellite(&rewritten).is_none(),
                "rewritten command must be local: {rewritten:?}"
            );
        }
    }

    // --- session: command remap ------------------------------------------

    #[test]
    fn post_negotiation_hello_ok_is_fatal_to_relay_session() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let error = session
            .handle_inbound(&encode(&FrameKind::HelloOk {
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                server_caps: phux_protocol::caps::ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile: phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            }))
            .expect_err("duplicate HELLO_OK must tear down the relay");
        assert!(error.contains("direction-invalid"));
    }
    fn native_profile() -> BootstrapProfile {
        BootstrapProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
            features: phux_protocol::caps::EngineFeatureSet::required_native(),
        }
    }

    fn native_stream_profile() -> BootstrapStreamProfile {
        BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
        }
    }

    #[test]
    fn relay_rejects_bootstrap_profile_mismatch_before_fanout() {
        let mut session =
            RelaySession::new_negotiated(host(), BootstrapLimits::default(), native_profile());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        attach(&mut session, 7, ClientId(1), out_tx);
        let error = session
            .handle_inbound(&encode(&FrameKind::BootstrapBegin {
                terminal_id: TerminalId::local(7),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            }))
            .expect_err("profile mismatch must fail the link");
        assert!(error.contains("negotiated"));
        assert!(out_rx.try_recv().is_err(), "mismatched BEGIN leaked");
        assert!(session.bootstrap_flows.is_empty());
    }

    #[test]
    fn relay_rejects_gapped_chunks_and_live_delta_before_ready() {
        let stream_id = StreamId::new(2).expect("stream");
        let bootstrap_id = BootstrapId::new(3).expect("bootstrap");

        let mut gapped =
            RelaySession::new_negotiated(host(), BootstrapLimits::default(), native_profile());
        gapped
            .handle_inbound(&encode(&FrameKind::BootstrapBegin {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                profile: native_stream_profile(),
                cols: 80,
                rows: 24,
                base_seq: 10,
            }))
            .expect("valid BEGIN");
        let error = gapped
            .handle_inbound(&encode(&FrameKind::BootstrapChunk {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                chunk_seq: 1,
                payload: bytes::Bytes::from_static(b"opaque"),
            }))
            .expect_err("gapped chunk must fail the link");
        assert!(error.contains("expected 0"));

        let mut early_live =
            RelaySession::new_negotiated(host(), BootstrapLimits::default(), native_profile());
        early_live
            .handle_inbound(&encode(&FrameKind::BootstrapBegin {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                profile: native_stream_profile(),
                cols: 80,
                rows: 24,
                base_seq: 10,
            }))
            .expect("valid BEGIN");
        let error = early_live
            .handle_inbound(&encode(&FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                seq: 11,
                bytes: bytes::Bytes::from_static(b"x"),
            }))
            .expect_err("live output before READY must fail the link");
        assert!(error.contains("before BOOTSTRAP_READY"));
    }

    #[test]
    fn relay_fans_out_complete_native_prefix_before_live_delta() {
        let mut session =
            RelaySession::new_negotiated(host(), BootstrapLimits::default(), native_profile());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        attach(&mut session, 7, ClientId(1), out_tx);
        let stream_id = StreamId::new(4).expect("stream");
        let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                profile: native_stream_profile(),
                cols: 80,
                rows: 24,
                base_seq: 20,
            },
            FrameKind::BootstrapChunk {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"opaque"),
            },
            FrameKind::BootstrapReady {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                seq: 21,
                bytes: bytes::Bytes::from_static(b"live"),
            },
        ] {
            session
                .handle_inbound(&encode(&frame))
                .expect("ordered native bootstrap frame");
        }
        assert!(matches!(
            out_rx.try_recv().expect("BEGIN"),
            Outbound::Frame(FrameKind::BootstrapBegin { .. })
        ));
        assert!(matches!(
            out_rx.try_recv().expect("CHUNK"),
            Outbound::Frame(FrameKind::BootstrapChunk { .. })
        ));
        assert!(matches!(
            out_rx.try_recv().expect("READY"),
            Outbound::Frame(FrameKind::BootstrapReady { .. })
        ));
        assert!(matches!(
            out_rx.try_recv().expect("live"),
            Outbound::Frame(FrameKind::TerminalOutput { seq: 21, .. })
        ));
        let flow = session.bootstrap_flows.get(&7).expect("active generation");
        assert!(flow.ready);
        assert_eq!(flow.stream_id, stream_id);
        assert_eq!(flow.bootstrap_id, bootstrap_id);
        assert_eq!(flow.profile, native_stream_profile());
    }

    #[test]
    fn content_subscription_requires_exact_downstream_profile_and_bounds() {
        let limits = BootstrapLimits::default();
        let mut session = RelaySession::new_negotiated(host(), limits, native_profile());
        let (out_tx, _out_rx) = mpsc::channel(8);
        let (reply, mut reply_rx) = oneshot::channel();
        let wire = session.handle_request_checked(RelayRequest::Command {
            command: Command::AttachTerminal {
                terminal_id: TerminalId::local(7),
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 7,
                client: ClientId(1),
                out_tx,
                seq: 1,
                awaits_snapshot: true,
                bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: Some(limits),
            }),
        });
        assert!(
            wire.is_none(),
            "mismatched content request reached the link"
        );
        assert!(session.subscribers.is_empty());
        assert!(session.pending.is_empty());
        let result = reply_rx.try_recv().expect("typed local refusal");
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::CodecUnavailable,
                ref message,
            } if message.contains("transcoding is unavailable")
                && message.contains("NativeState")
                && message.contains("SynthesizedVtRaw")
        ));

        let smaller = BootstrapLimits::new(64 * 1024, 128 * 1024).expect("valid bounds");
        let (out_tx, _out_rx) = mpsc::channel(8);
        let (reply, mut reply_rx) = oneshot::channel();
        let wire = session.handle_request_checked(RelayRequest::Command {
            command: Command::AttachTerminal {
                terminal_id: TerminalId::local(8),
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 8,
                client: ClientId(2),
                out_tx,
                seq: 1,
                awaits_snapshot: true,
                bootstrap_profile: Some(native_profile()),
                bootstrap_limits: Some(smaller),
            }),
        });
        assert!(wire.is_none(), "mismatched bounds reached the link");
        assert!(matches!(
            reply_rx.try_recv().expect("typed bounds refusal"),
            CommandResult::Error {
                code: ErrorCode::CodecUnavailable,
                ..
            }
        ));
    }

    #[test]
    fn relay_rejects_identity_change_after_ready_without_fanout_or_mutation() {
        let mut session =
            RelaySession::new_negotiated(host(), BootstrapLimits::default(), native_profile());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        attach(&mut session, 7, ClientId(1), out_tx);
        let stream_id = StreamId::new(4).expect("stream");
        let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                profile: native_stream_profile(),
                cols: 80,
                rows: 24,
                base_seq: 20,
            },
            FrameKind::BootstrapReady {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            session
                .handle_inbound(&encode(&frame))
                .expect("valid bootstrap prefix");
        }
        let _ = out_rx.try_recv().expect("BEGIN");
        let _ = out_rx.try_recv().expect("READY");
        let before = session.bootstrap_flows[&7];
        let error = session
            .handle_inbound(&encode(&FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(7),
                stream_id,
                bootstrap_id: BootstrapId::new(6).expect("different bootstrap"),
                seq: 21,
                bytes: bytes::Bytes::from_static(b"invalid"),
            }))
            .expect_err("identity change must terminate the link");
        assert!(error.contains("non-active bootstrap identity"));
        assert!(out_rx.try_recv().is_err(), "invalid frame reached consumer");
        assert_eq!(session.bootstrap_flows[&7], before);
    }

    #[test]
    fn generation_byte_budget_fails_without_allocating_chunk_payloads() {
        let mut session =
            RelaySession::new_negotiated(host(), BootstrapLimits::default(), native_profile());
        let stream_id = StreamId::new(1).expect("stream");
        let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
        session
            .begin_bootstrap_flow(7, stream_id, bootstrap_id, native_stream_profile())
            .expect("begin");
        for chunk_seq in 0..64 {
            session
                .accept_bootstrap_chunk(7, stream_id, bootstrap_id, chunk_seq, 256 * 1024)
                .expect("within 16 MiB generation budget");
        }
        let error = session
            .accept_bootstrap_chunk(7, stream_id, bootstrap_id, 64, 256 * 1024)
            .expect_err("generation above 16 MiB must fail");
        assert!(error.contains("per-generation bootstrap byte budget"));
    }

    #[test]
    fn saturated_open_closed_and_retained_subscribers_are_reaped() {
        let mut open = RelaySession::new(host(), BootstrapLimits::default());
        let (open_tx, mut open_rx) = mpsc::channel(1);
        subscribe(&mut open, 9, ClientId(1), open_tx.clone());
        open_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill open mailbox");
        open.fan_out(
            9,
            &FrameKind::Event {
                terminal: Some(TerminalId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
            },
        );
        assert!(!open.subscribers.contains_key(&9));
        let _ = open_rx.try_recv().expect("filler remains");

        let mut closed = RelaySession::new(host(), BootstrapLimits::default());
        let (closed_tx, closed_rx) = mpsc::channel(1);
        subscribe(&mut closed, 9, ClientId(1), closed_tx);
        drop(closed_rx);
        closed.fan_out(
            9,
            &FrameKind::Event {
                terminal: Some(TerminalId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
            },
        );
        assert!(!closed.subscribers.contains_key(&9));

        let mut retained = RelaySession::new(host(), BootstrapLimits::default());
        let (retained_tx, _retained_rx) = mpsc::channel(1);
        subscribe(&mut retained, 9, ClientId(1), retained_tx.clone());
        retained_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill retained mailbox");
        for chunk_seq in 0..=MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES {
            retained.fan_out(
                9,
                &FrameKind::BootstrapChunk {
                    terminal_id: TerminalId::satellite("devbox", 9),
                    stream_id: StreamId::new(1).expect("stream"),
                    bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                    chunk_seq: u32::try_from(chunk_seq).expect("small sequence"),
                    payload: bytes::Bytes::from_static(b"x"),
                },
            );
        }
        assert!(!retained.subscribers.contains_key(&9));
        assert_eq!(retained.retained_bytes, 0);
        assert_eq!(retained.retained_frames, 0);
    }

    #[test]
    fn connection_wide_retention_budget_reaps_excess_subscriber() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let mut receivers = Vec::new();
        for client in 1..=9 {
            let (tx, rx) = mpsc::channel(1);
            subscribe(&mut session, 9, ClientId(client), tx.clone());
            tx.try_send(Outbound::Frame(FrameKind::Detach))
                .expect("fill consumer mailbox");
            receivers.push(rx);
        }
        let payload = bytes::Bytes::from(vec![0; 1024 * 1024 - RETAINED_FRAME_OVERHEAD]);
        session.fan_out(
            9,
            &FrameKind::BootstrapChunk {
                terminal_id: TerminalId::satellite("devbox", 9),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                chunk_seq: 0,
                payload,
            },
        );
        assert_eq!(session.subscribers[&9].len(), 8);
        assert_eq!(session.retained_bytes, MAX_RELAY_CONNECTION_RETAINED_BYTES);
        assert_eq!(session.retained_frames, 8);
        assert_eq!(receivers.len(), 9);
    }

    #[test]
    fn session_remaps_request_ids_and_resolves_replies() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply_a, mut rx_a) = oneshot::channel();
        let (reply_b, mut rx_b) = oneshot::channel();

        let wire_a = session.handle_request(RelayRequest::Command {
            command: Command::GetState {
                scope: StateScope::Server,
            },
            reply: reply_a,
            subscribe: None,
        });
        let wire_b = session.handle_request(RelayRequest::Command {
            command: Command::Upgrade,
            reply: reply_b,
            subscribe: None,
        });

        let FrameKind::Command {
            request_id: id_a, ..
        } = decode(&wire_a)
        else {
            panic!("expected COMMAND on the wire");
        };
        let FrameKind::Command {
            request_id: id_b, ..
        } = decode(&wire_b)
        else {
            panic!("expected COMMAND on the wire");
        };
        assert_ne!(id_a, id_b, "link-side request ids must be distinct");

        // Resolve out of order: the remap must correlate, not FIFO.
        session
            .handle_inbound(&encode(&FrameKind::CommandResult {
                request_id: id_b,
                result: CommandResult::OkWith(CommandValue::Json("b".to_owned())),
            }))
            .expect("valid satellite frame");
        session
            .handle_inbound(&encode(&FrameKind::CommandResult {
                request_id: id_a,
                result: CommandResult::Ok,
            }))
            .expect("valid satellite frame");

        assert_eq!(rx_a.try_recv().expect("a resolved"), CommandResult::Ok);
        assert_eq!(
            rx_b.try_recv().expect("b resolved"),
            CommandResult::OkWith(CommandValue::Json("b".to_owned()))
        );
    }

    #[test]
    fn session_maps_correlated_error_frames_to_command_errors() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, mut rx) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::Upgrade,
            reply,
            subscribe: None,
        });
        let FrameKind::Command { request_id, .. } = decode(&wire) else {
            panic!("expected COMMAND");
        };
        session
            .handle_inbound(&encode(&FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::TerminalNotFound,
                message: "nope".to_owned(),
            }))
            .expect("valid satellite frame");
        assert_eq!(
            rx.try_recv().expect("resolved"),
            CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "nope".to_owned(),
            }
        );
    }

    // --- session: spawn relay (phux-v45.6) --------------------------------

    fn spawn_request(reply: oneshot::Sender<SpawnResult>) -> RelayRequest {
        RelayRequest::Spawn {
            spawn: SatelliteSpawn {
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                owner_terminal: None,
                initial_size: None,
                resource: None,
            },
            reply,
        }
    }

    #[test]
    fn session_relays_spawn_with_stripped_addressing_and_retags_the_reply() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, mut rx) = oneshot::channel();
        let wire = session.handle_request(spawn_request(reply));
        let FrameKind::SpawnTerminal {
            request_id,
            satellite,
            ..
        } = decode(&wire)
        else {
            panic!("expected SPAWN_TERMINAL on the wire");
        };
        assert_eq!(
            satellite, None,
            "the addressing field never crosses the link (no chaining)"
        );
        // The satellite answers with its Local id; the consumer sees it
        // re-tagged with this link's host.
        session
            .handle_inbound(&encode(&FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Ok(TerminalId::local(42)),
            }))
            .expect("valid satellite frame");
        assert_eq!(
            rx.try_recv().expect("spawn resolved"),
            SpawnResult::Ok(TerminalId::satellite("devbox", 42))
        );
    }

    #[test]
    fn session_rejects_chained_ids_and_relays_spawn_errors_verbatim() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        // A Satellite-tagged id in the satellite's own reply never chains.
        let (reply, mut rx) = oneshot::channel();
        let wire = session.handle_request(spawn_request(reply));
        let FrameKind::SpawnTerminal { request_id, .. } = decode(&wire) else {
            panic!("expected SPAWN_TERMINAL");
        };
        session
            .handle_inbound(&encode(&FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Ok(TerminalId::satellite("nested", 7)),
            }))
            .expect("valid satellite frame");
        assert!(matches!(
            rx.try_recv().expect("resolved"),
            SpawnResult::Err(SpawnError::SpawnFailed(_))
        ));
        // A typed satellite-side error relays verbatim.
        let (reply, mut rx) = oneshot::channel();
        let wire = session.handle_request(spawn_request(reply));
        let FrameKind::SpawnTerminal { request_id, .. } = decode(&wire) else {
            panic!("expected SPAWN_TERMINAL");
        };
        session
            .handle_inbound(&encode(&FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Err(SpawnError::GroupNotFound),
            }))
            .expect("valid satellite frame");
        assert_eq!(
            rx.try_recv().expect("resolved"),
            SpawnResult::Err(SpawnError::GroupNotFound)
        );
    }

    #[tokio::test]
    async fn handle_and_session_preserve_spawn_owner_geometry_and_pty_options() {
        let (handle, mut mailbox) = RelayHandle::new(host());
        let spawn = SatelliteSpawn {
            group: GroupId::new(7),
            command: Some(vec!["/bin/cat".to_owned()]),
            cwd: Some("/work".to_owned()),
            env: Some(vec![("SPLIT".to_owned(), "yes".to_owned())]),
            term: Some("xterm-256color".to_owned()),
            owner_terminal: Some(91),
            initial_size: Some((132, 43)),
            resource: None,
        };
        let expected = FrameKind::SpawnTerminal {
            request_id: 0,
            group: spawn.group,
            command: spawn.command.clone(),
            cwd: spawn.cwd.clone(),
            env: spawn.env.clone(),
            term: spawn.term.clone(),
            satellite: None,
            owner_terminal: Some(TerminalId::local(91)),
            agent_session: None,
            initial_size: Some((132, 43)),
            resource: None,
        };
        let consumer = handle.spawn(spawn);
        let satellite = async {
            let request = mailbox.requests.recv().await.expect("spawn enqueued");
            let mut session = RelaySession::new(host(), BootstrapLimits::default());
            let mut frame = decode(&session.handle_request(request));
            let FrameKind::SpawnTerminal { request_id, .. } = &mut frame else {
                panic!("spawn frame");
            };
            let link_request_id = *request_id;
            *request_id = 0;
            assert_eq!(frame, expected);
            session
                .handle_inbound(&encode(&FrameKind::TerminalSpawned {
                    request_id: link_request_id,
                    result: SpawnResult::Ok(TerminalId::local(92)),
                }))
                .expect("spawn reply");
        };
        let (result, ()) = tokio::join!(consumer, satellite);
        assert_eq!(result, SpawnResult::Ok(TerminalId::satellite("devbox", 92)));
    }

    #[test]
    fn teardown_and_fail_fast_resolve_spawns_with_satellite_unreachable() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, mut rx) = oneshot::channel();
        let _ = session.handle_request(spawn_request(reply));
        session.teardown("satellite went away");
        assert!(matches!(
            rx.try_recv().expect("pending spawn failed"),
            SpawnResult::Err(SpawnError::SatelliteUnreachable(_))
        ));

        let (reply, mut rx) = oneshot::channel();
        fail_fast(spawn_request(reply), &host(), "backoff");
        assert!(matches!(
            rx.try_recv().expect("spawn failed fast"),
            SpawnResult::Err(SpawnError::SatelliteUnreachable(_))
        ));
    }

    #[test]
    fn prune_abandoned_covers_pending_spawns() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, rx) = oneshot::channel();
        let _ = session.handle_request(spawn_request(reply));
        assert_eq!(session.prune_abandoned(), 0, "consumer still waits");
        drop(rx);
        assert_eq!(session.prune_abandoned(), 1, "abandoned spawn pruned");
    }

    // --- session: return-leg re-tagging ----------------------------------

    #[test]
    fn session_retags_subscribed_streams_local_to_satellite() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);

        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        session
            .handle_inbound(&encode(&FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(9),
                stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
                seq: 42,
                bytes: bytes::Bytes::from_static(b"hi"),
            }))
            .expect("valid satellite frame");
        // A different terminal: nothing must reach the subscriber.
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(10)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");

        let Outbound::Frame(first) = out_rx.try_recv().expect("event fanned out") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            first,
            FrameKind::Event {
                terminal: Some(TerminalId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
            }
        );
        let Outbound::Frame(second) = out_rx.try_recv().expect("output fanned out") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(matches!(
            second,
            FrameKind::TerminalOutput { terminal_id, seq: 42, .. }
                if terminal_id == TerminalId::satellite("devbox", 9)
        ));
        assert!(out_rx.try_recv().is_err(), "unsubscribed terminal leaked");
    }

    #[test]
    fn session_drops_chained_satellite_tags_from_the_return_leg() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        // ADR-0007: satellites are unaware of each other; a nested
        // Satellite tag must never be re-relayed.
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::satellite("nested", 9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        assert!(out_rx.try_recv().is_err());
    }

    #[test]
    fn terminal_closed_retags_and_drops_the_proxy_subscription() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        session
            .handle_inbound(&encode(&FrameKind::TerminalClosed {
                terminal_id: TerminalId::local(9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("closed fanned out") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::TerminalClosed {
                terminal_id: TerminalId::satellite("devbox", 9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
            }
        );
        // Subscription is gone: further frames for id 9 do not fan out.
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        assert!(out_rx.try_recv().is_err());
    }

    // --- attach snapshot ordering under backpressure (phux-v45.12) --------

    fn snapshot_frame(id: u32) -> FrameKind {
        FrameKind::BootstrapReady {
            terminal_id: TerminalId::local(id),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            history_cursor: None,
        }
    }

    fn begin_frame(id: u32, generation: u64) -> FrameKind {
        FrameKind::BootstrapBegin {
            terminal_id: TerminalId::local(id),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(generation)
                .expect("test bootstrap id"),
            profile: phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        }
    }

    fn output_frame(id: u32, seq: u64, bytes: &'static [u8]) -> FrameKind {
        FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(id),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            seq,
            bytes: bytes::Bytes::from_static(bytes),
        }
    }

    #[tokio::test]
    async fn full_mailbox_retains_the_snapshot_so_a_delta_never_overtakes_it() {
        // L1 §9.1: the snapshot MUST precede the first delta. When the
        // consumer's mailbox is briefly full at attach the return-leg
        // snapshot cannot be delivered; it must be retained (not dropped)
        // so a later TERMINAL_OUTPUT does not reach the consumer first.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        // Capacity two so both the retried snapshot and the delta can land
        // in order once the fillers drain.
        let (out_tx, mut out_rx) = mpsc::channel(2);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        // Saturate the mailbox: the snapshot's fan-out will be refused.
        out_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("filler one");
        out_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("filler two");

        // Snapshot arrives while saturated -> retained, nothing delivered.
        session
            .handle_inbound(&encode(&snapshot_frame(9)))
            .expect("valid satellite frame");
        // Free the mailbox.
        assert!(matches!(
            out_rx.try_recv().expect("filler one drains"),
            Outbound::Frame(FrameKind::Detach)
        ));
        assert!(matches!(
            out_rx.try_recv().expect("filler two drains"),
            Outbound::Frame(FrameKind::Detach)
        ));

        // A later OUTPUT delta must flush the retained snapshot FIRST and
        // only then ride after it.
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"delta")))
            .expect("valid satellite frame");

        let Outbound::Frame(first) = out_rx.try_recv().expect("snapshot delivered") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(
            matches!(first, FrameKind::BootstrapReady { .. }),
            "the snapshot must reach the consumer before any delta, got {first:?}"
        );
        let Outbound::Frame(second) = out_rx.try_recv().expect("delta delivered") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(
            matches!(
                second,
                FrameKind::TerminalOutput { ref terminal_id, seq: 1, .. }
                    if *terminal_id == TerminalId::satellite("devbox", 9)
            ),
            "the delta must ride after the snapshot, re-tagged, got {second:?}"
        );
    }

    #[tokio::test]
    async fn deltas_are_suppressed_until_the_retained_snapshot_flushes_on_the_tick() {
        // While the snapshot stays stuck behind a full mailbox, deltas are
        // suppressed (never delivered ahead of it); the keepalive-tick flush
        // converges the consumer once the mailbox drains.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(1);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        out_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("filler");

        // Snapshot refused (retained), then a delta while still full.
        session
            .handle_inbound(&encode(&snapshot_frame(9)))
            .expect("valid satellite frame");
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"delta")))
            .expect("valid satellite frame");

        // Only the filler is queued: neither the snapshot nor the delta
        // reached the consumer (the delta was suppressed, not reordered).
        assert!(matches!(
            out_rx.try_recv().expect("filler drains"),
            Outbound::Frame(FrameKind::Detach)
        ));
        assert!(
            out_rx.try_recv().is_err(),
            "no frame may reach the consumer while the snapshot is stuck"
        );

        // The keepalive tick retries the retained snapshot; the mailbox now
        // has room, so it lands — and it was never preceded by the delta.
        session.flush_pending_snapshots();
        let Outbound::Frame(frame) = out_rx.try_recv().expect("snapshot flushed on tick") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(
            matches!(frame, FrameKind::BootstrapReady { .. }),
            "the tick flush delivers the retained snapshot, got {frame:?}"
        );
        assert!(
            out_rx.try_recv().is_err(),
            "the suppressed delta was dropped, not delivered before the snapshot"
        );
    }

    #[tokio::test]
    async fn a_fresher_snapshot_replaces_a_retained_one() {
        // A satellite resync sends a newer snapshot while an older one is
        // still retained: the freshest full-grid must win.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(1);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        out_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("filler");

        // First snapshot refused and retained.
        session
            .handle_inbound(&encode(&begin_frame(9, 1)))
            .expect("valid satellite frame");
        // A fresher snapshot arrives (still full) and must replace it.
        session
            .handle_inbound(&encode(&begin_frame(9, 2)))
            .expect("valid satellite frame");
        assert!(matches!(
            out_rx.try_recv().expect("filler drains"),
            Outbound::Frame(FrameKind::Detach)
        ));
        session.flush_pending_snapshots();
        let Outbound::Frame(FrameKind::BootstrapBegin { bootstrap_id, .. }) =
            out_rx.try_recv().expect("bootstrap BEGIN flushed")
        else {
            panic!("expected a bootstrap BEGIN");
        };
        assert_eq!(
            bootstrap_id.get(),
            2,
            "the freshest retained generation must win"
        );
    }

    // --- second-subscriber attach ordering (phux-v45.14) ------------------

    #[test]
    fn a_second_attach_sees_its_snapshot_before_any_delta_of_the_ongoing_stream() {
        // The core phux-v45.14 fix. Consumer A is already attached and
        // streaming; consumer B attaches to the same satellite terminal. B's
        // registration lands immediately, but its own return-leg
        // TERMINAL_SNAPSHOT arrives ~1 RTT after A's ongoing TERMINAL_OUTPUT.
        // B must NOT observe that delta before its snapshot (L1 §9.1).
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (tx_a, mut rx_a) = mpsc::channel(8);
        let (tx_b, mut rx_b) = mpsc::channel(8);

        // A attaches and its snapshot lands: A is now streaming (gate Open).
        attach(&mut session, 9, ClientId(1), tx_a);
        session
            .handle_inbound(&encode(&snapshot_frame(9)))
            .expect("valid satellite frame");
        let Outbound::Frame(a_snap) = rx_a.try_recv().expect("A's snapshot") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(matches!(a_snap, FrameKind::BootstrapReady { .. }));

        // B attaches to the same terminal (registration is immediate) but its
        // snapshot has not been requested/answered yet.
        attach(&mut session, 9, ClientId(2), tx_b);

        // A's stream produces a delta before B's snapshot arrives. It fans
        // out to both subscribers — A (Open) receives it; B (AwaitingFirst)
        // must have it suppressed.
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"a-stream")))
            .expect("valid satellite frame");
        let Outbound::Frame(a_delta) = rx_a.try_recv().expect("A sees the delta") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(matches!(a_delta, FrameKind::TerminalOutput { seq: 1, .. }));
        assert!(
            rx_b.try_recv().is_err(),
            "B must not see a delta before its own snapshot (L1 §9.1)"
        );

        // B's own snapshot finally lands: it is delivered, opening B's gate.
        session
            .handle_inbound(&encode(&snapshot_frame(9)))
            .expect("valid satellite frame");
        let Outbound::Frame(b_first) = rx_b.try_recv().expect("B's snapshot lands") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(
            matches!(b_first, FrameKind::BootstrapReady { .. }),
            "B's first frame must be its snapshot, got {b_first:?}"
        );

        // A subsequent delta now rides after B's snapshot, in order.
        session
            .handle_inbound(&encode(&output_frame(9, 2, b"after")))
            .expect("valid satellite frame");
        let Outbound::Frame(b_delta) = rx_b.try_recv().expect("B sees the post-snapshot delta")
        else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(
            matches!(
                b_delta,
                FrameKind::TerminalOutput { ref terminal_id, seq: 2, .. }
                    if *terminal_id == TerminalId::satellite("devbox", 9)
            ),
            "B's delta must follow its snapshot, re-tagged, got {b_delta:?}"
        );
    }

    #[test]
    fn a_gated_attach_still_receives_terminal_closed_before_being_reaped() {
        // phux-v45.14 sub-finding (a): a subscriber still awaiting its first
        // snapshot is reaped when the terminal closes. TERMINAL_CLOSED must
        // be delivered best-effort past the gate, or the consumer is torn
        // down without ever learning its terminal is gone.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (tx_b, mut rx_b) = mpsc::channel(8);
        // B attaches: gate is AwaitingFirst, no snapshot delivered yet.
        attach(&mut session, 9, ClientId(2), tx_b);

        // A normal delta is still suppressed while gated...
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"suppressed")))
            .expect("valid satellite frame");
        assert!(
            rx_b.try_recv().is_err(),
            "a content delta is suppressed before the snapshot"
        );

        // ...but the terminal closing must reach B before it is reaped.
        session
            .handle_inbound(&encode(&FrameKind::TerminalClosed {
                terminal_id: TerminalId::local(9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = rx_b.try_recv().expect("close delivered past the gate") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::TerminalClosed {
                terminal_id: TerminalId::satellite("devbox", 9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
            },
            "a gated subscriber must still see TERMINAL_CLOSED"
        );
        // And the subscription is reaped: no further fan-out for the id.
        session
            .handle_inbound(&encode(&output_frame(9, 2, b"after-close")))
            .expect("valid satellite frame");
        assert!(
            rx_b.try_recv().is_err(),
            "subscription must be reaped on close"
        );
    }

    #[test]
    fn an_event_only_subscription_is_not_gated_by_the_snapshot() {
        // A SUBSCRIBE_EVENTS / SUBSCRIBE_TERMINAL_EVENTS registration carries
        // no snapshot: its EVENT deltas must flow immediately (gating them
        // would strand the subscriber forever, since no snapshot ever comes).
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("event flows without a snapshot")
        else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::Event {
                terminal: Some(TerminalId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
            }
        );
    }

    #[test]
    fn subscribe_then_attach_upgrade_gates_deltas_until_the_attach_snapshot() {
        // phux-v45.15 edge (1). A client is already event-subscribed to a
        // satellite terminal (gate Open, its stream flowing) and then UPGRADES
        // to an attach on the SAME terminal. The upgrade must re-gate the
        // stream to AwaitingFirst so the attach's content deltas cannot ride
        // ahead of the attach's own snapshot (L1 §9.1) — the same guarantee a
        // fresh second attach gets (phux-v45.14), resurfacing on the upgrade
        // path. Without the re-gate the delta at step 3 leaks, so this test is
        // non-vacuous.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);

        // 1. Event-only subscribe: gate Open, an EVENT flows immediately.
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        assert!(
            out_rx.try_recv().is_ok(),
            "the event-only stream must be flowing before the upgrade"
        );

        // 2. UPGRADE the same client to an attach on the same terminal.
        attach(&mut session, 9, ClientId(1), out_tx);

        // 3. A content delta arrives before the attach's snapshot. It must now
        //    be suppressed — the upgrade re-gated the stream.
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"pre-snapshot")))
            .expect("valid satellite frame");
        assert!(
            out_rx.try_recv().is_err(),
            "the upgrade must gate deltas until its own snapshot (L1 §9.1)"
        );

        // 4. The attach's snapshot lands: delivered, re-opening the gate.
        session
            .handle_inbound(&encode(&snapshot_frame(9)))
            .expect("valid satellite frame");
        let Outbound::Frame(first) = out_rx.try_recv().expect("the attach snapshot lands") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(
            matches!(first, FrameKind::BootstrapReady { .. }),
            "the first post-upgrade frame must be the snapshot, got {first:?}"
        );

        // 5. A subsequent delta now rides after the snapshot, in order.
        session
            .handle_inbound(&encode(&output_frame(9, 2, b"post-snapshot")))
            .expect("valid satellite frame");
        let Outbound::Frame(delta) = out_rx.try_recv().expect("post-snapshot delta rides") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(
            matches!(
                delta,
                FrameKind::TerminalOutput { ref terminal_id, seq: 2, .. }
                    if *terminal_id == TerminalId::satellite("devbox", 9)
            ),
            "the delta must follow the snapshot, re-tagged, got {delta:?}"
        );
    }

    #[test]
    fn an_event_only_re_subscribe_does_not_re_gate_a_flowing_stream() {
        // The re-gate is scoped to an attach UPGRADE (a snapshot-bearing
        // re-subscribe). An event-only re-subscribe carries no snapshot, so it
        // must leave an already-Open stream flowing — re-gating it would strand
        // the consumer forever (no snapshot ever comes to re-open the gate).
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        // A second event-only subscribe for the same client (idempotent).
        subscribe(&mut session, 9, ClientId(1), out_tx);
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        assert!(
            out_rx.try_recv().is_ok(),
            "an event-only re-subscribe must not re-gate the flowing stream"
        );
    }

    #[test]
    fn a_gated_attach_still_receives_a_bell_past_the_gate() {
        // phux-v45.15 edge (2). A BELL is an ephemeral notification the
        // snapshot does not capture, so gating it behind an AwaitingFirst
        // subscriber's not-yet-delivered snapshot would drop it permanently.
        // It routes past the gate best-effort, like TERMINAL_CLOSED.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (tx, mut rx) = mpsc::channel(8);
        // Attach: gate is AwaitingFirst, no snapshot delivered yet.
        attach(&mut session, 9, ClientId(2), tx);

        // A content delta is suppressed while gated...
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"suppressed")))
            .expect("valid satellite frame");
        assert!(
            rx.try_recv().is_err(),
            "a content delta is suppressed before the snapshot"
        );

        // ...but a bell rings through, re-tagged, past the gate.
        session
            .handle_inbound(&encode(&FrameKind::Bell {
                terminal_id: TerminalId::local(9),
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = rx.try_recv().expect("bell delivered past the gate") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::Bell {
                terminal_id: TerminalId::satellite("devbox", 9),
            },
            "a gated subscriber must still see a BELL"
        );

        // The gate is still closed: a further delta stays suppressed until the
        // snapshot lands (the bell bypass does not open the gate).
        session
            .handle_inbound(&encode(&output_frame(9, 2, b"still-gated")))
            .expect("valid satellite frame");
        assert!(
            rx.try_recv().is_err(),
            "the bell bypass must not open the snapshot gate"
        );
    }

    // --- session: lifecycle teardown --------------------------------------

    #[test]
    fn teardown_fails_pending_and_notifies_each_consumer_once() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, mut reply_rx) = oneshot::channel();
        let _ = session.handle_request(RelayRequest::Command {
            command: Command::Upgrade,
            reply,
            subscribe: None,
        });
        let (out_tx, mut out_rx) = mpsc::channel(8);
        // Two subscriptions for the same client: one notification.
        subscribe(&mut session, 1, ClientId(7), out_tx.clone());
        subscribe(&mut session, 2, ClientId(7), out_tx);

        session.teardown("satellite went away");

        assert!(matches!(
            reply_rx.try_recv().expect("pending failed"),
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
        let Outbound::Frame(frame) = out_rx.try_recv().expect("consumer notified") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(matches!(
            frame,
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
        assert!(
            out_rx.try_recv().is_err(),
            "one typed notification per consumer, not per subscription"
        );
    }

    #[test]
    fn unsubscribe_client_stops_fan_out() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        let frames = session.handle_unsubscribe(Unsubscribe::Client(ClientId(1)));
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        assert!(out_rx.try_recv().is_err());
        // phux-v45.11 finding 4: the last proxy subscriber left, so the
        // session tells the satellite to stop streaming the terminal.
        assert_eq!(frames.len(), 1, "one satellite-side detach expected");
        let FrameKind::Command { command, .. } = decode(&frames[0]) else {
            panic!("expected COMMAND on the wire");
        };
        assert_eq!(
            command,
            Command::DetachTerminal {
                terminal_id: TerminalId::local(9),
            }
        );
    }

    // --- lifecycle hardening (phux-v45.11) ---------------------------------

    #[test]
    fn unsubscribe_keeps_satellite_streaming_while_other_subscribers_remain() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (tx_a, _rx_a) = mpsc::channel(8);
        let (tx_b, mut rx_b) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), tx_a);
        subscribe(&mut session, 9, ClientId(2), tx_b);
        // Client 1 detaches its terminal: client 2 still observes it, so
        // no satellite-side DETACH_TERMINAL may be emitted (it would tear
        // down the link's single shared stream under client 2).
        let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
            client: ClientId(1),
            terminal: 9,
            seq: 2,
            reply: None,
        });
        assert!(
            frames.is_empty(),
            "satellite-side detach must wait for the last subscriber"
        );
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(_) = rx_b.try_recv().expect("client 2 still fanned out") else {
            panic!("unexpected terminal outbound sentinel");
        };
        // Now the last subscriber leaves: exactly one detach goes out.
        let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
            client: ClientId(2),
            terminal: 9,
            seq: 2,
            reply: None,
        });
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn stale_terminal_unsubscribe_does_not_tear_down_a_fresher_reattach() {
        // phux-v45.7 reorder guard. A consumer's DETACH_TERMINAL rides the
        // unbounded unsubscribe channel; its immediate same-terminal
        // re-ATTACH rides the bounded request mailbox. The link session's
        // `select!` can drain the re-attach first, so by the time the
        // stale detach is applied a newer registration already exists.
        // The detach carries the token it was issued with (2); the live
        // registration carries the re-attach token (3), so the withdrawal
        // is dropped: no subscriber removed, no satellite-side
        // DETACH_TERMINAL emitted, and the re-attached stream keeps
        // flowing.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        // Original attach (token 1), then the re-attach that raced ahead
        // of the stale detach (token 3).
        subscribe_at(&mut session, 9, ClientId(1), 1, out_tx.clone());
        subscribe_at(&mut session, 9, ClientId(1), 3, out_tx);

        let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
            client: ClientId(1),
            terminal: 9,
            seq: 2,
            reply: None,
        });
        assert!(
            frames.is_empty(),
            "a stale detach must not emit a satellite-side DETACH for a re-attached terminal"
        );

        // The re-attached stream is intact.
        session
            .handle_inbound(&encode(&FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(9),
                stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
                seq: 7,
                bytes: bytes::Bytes::from_static(b"live"),
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("re-attached stream torn down")
        else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(matches!(
            frame,
            FrameKind::TerminalOutput { terminal_id, seq: 7, .. }
                if terminal_id == TerminalId::satellite("devbox", 9)
        ));

        // A genuine later detach (token newer than the registration) still
        // withdraws and detaches satellite-side.
        let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
            client: ClientId(1),
            terminal: 9,
            seq: 4,
            reply: None,
        });
        assert_eq!(frames.len(), 1, "an in-order detach still tears down");
    }

    #[tokio::test]
    async fn unsubscribe_survives_a_full_relay_mailbox() {
        // phux-v45.11 finding 1: unsubscribes ride a dedicated unbounded
        // channel, so a saturated request mailbox cannot drop them.
        let (handle, mut mailbox) = RelayHandle::new(host());
        for _ in 0..RELAY_MAILBOX {
            handle.forward(FrameKind::Detach);
        }
        handle.unsubscribe_client(ClientId(1));
        let detach = handle.unsubscribe_terminal(ClientId(2), 7);
        tokio::pin!(detach);
        assert!(futures_util::poll!(&mut detach).is_pending());
        assert!(matches!(
            mailbox.unsubscribes.try_recv().expect("delivered"),
            Unsubscribe::Client(ClientId(1))
        ));
        // The issue-order token is opaque here; match on the routing
        // fields (the reorder guard's semantics are covered separately).
        assert!(matches!(
            mailbox.unsubscribes.try_recv().expect("delivered"),
            Unsubscribe::Terminal {
                client: ClientId(2),
                terminal: 7,
                ..
            }
        ));
        assert_eq!(detach.await, CommandResult::Ok);
    }

    #[tokio::test]
    async fn withdrawal_receipt_fences_retained_replay_and_live_fanout() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(1);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        out_tx.try_send(Outbound::Frame(FrameKind::Detach)).unwrap();
        session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
        assert!(session.retained_bytes > 0);
        let (reply, received) = oneshot::channel();
        let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
            client: ClientId(1),
            terminal: 9,
            seq: 2,
            reply: Some(WithdrawalReceipt::new(reply)),
        });
        received.await.unwrap();
        assert_eq!(
            frames.len(),
            1,
            "last observer releases the upstream subscription"
        );
        assert_eq!(session.retained_bytes, 0);
        assert!(!session.subscribers.contains_key(&9));
        out_rx.try_recv().unwrap();
        session.flush_pending_snapshots();
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"after detach")))
            .unwrap();
        assert!(
            out_rx.try_recv().is_err(),
            "no retained or new terminal frame after receipt"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn withdrawal_timeout_cancels_unapplied_teardown() {
        let (handle, mut mailbox) = RelayHandle::new(host());
        let detach = handle.unsubscribe_terminal(ClientId(1), 9);
        tokio::pin!(detach);
        assert!(futures_util::poll!(&mut detach).is_pending());
        tokio::time::advance(RELAY_COMMAND_TIMEOUT).await;
        assert!(matches!(
            detach.await,
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, _out_rx) = mpsc::channel(1);
        subscribe_at(&mut session, 9, ClientId(1), 0, out_tx);
        let frames = session.handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap());
        assert!(
            frames.is_empty(),
            "safe refusal must cancel queued teardown"
        );
        assert!(session.subscribers.contains_key(&9));
    }

    #[tokio::test(start_paused = true)]
    async fn applied_withdrawal_wins_even_when_waiter_resumes_past_deadline() {
        let (handle, mut mailbox) = RelayHandle::new(host());
        let detach = handle.unsubscribe_terminal(ClientId(1), 9);
        tokio::pin!(detach);
        assert!(futures_util::poll!(&mut detach).is_pending());
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, _out_rx) = mpsc::channel(8);
        subscribe_at(&mut session, 9, ClientId(1), 0, out_tx);
        assert_eq!(
            session
                .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
                .len(),
            1
        );
        tokio::time::advance(RELAY_COMMAND_TIMEOUT).await;
        assert_eq!(
            detach.await,
            CommandResult::Ok,
            "a committed removal is never a safe refusal"
        );
        assert!(!session.subscribers.contains_key(&9));
    }

    #[tokio::test]
    async fn abandoned_withdrawal_still_cleans_up_without_a_timeout_refusal() {
        let (handle, mut mailbox) = RelayHandle::new(host());
        let mut detach = Box::pin(handle.unsubscribe_terminal(ClientId(1), 9));
        assert!(futures_util::poll!(&mut detach).is_pending());
        drop(detach);
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, _out_rx) = mpsc::channel(8);
        subscribe_at(&mut session, 9, ClientId(1), 0, out_tx);
        assert_eq!(
            session
                .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
                .len(),
            1
        );
        assert!(
            !session.subscribers.contains_key(&9),
            "disconnected callers still get undroppable cleanup"
        );
    }

    #[tokio::test]
    async fn withdrawal_without_a_live_session_is_idempotent() {
        let (handle, mut mailbox) = RelayHandle::new(host());
        let detach = handle.unsubscribe_terminal(ClientId(1), 9);
        tokio::pin!(detach);
        assert!(futures_util::poll!(&mut detach).is_pending());
        // Mirrors the supervisor's disconnected/refused/backoff drain: there
        // is no session, so dropping the receipt certifies no proxy can emit.
        drop(mailbox.unsubscribes.try_recv().unwrap());
        assert_eq!(detach.await, CommandResult::Ok);
        drop(mailbox);
        assert_eq!(
            handle.unsubscribe_terminal(ClientId(1), 9).await,
            CommandResult::Ok
        );
    }

    #[tokio::test]
    async fn stale_withdrawal_is_refused_while_the_newer_proxy_stays_live() {
        let (handle, mut mailbox) = RelayHandle::new(host());
        let detach = handle.unsubscribe_terminal(ClientId(1), 9);
        tokio::pin!(detach);
        assert!(futures_util::poll!(&mut detach).is_pending());
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        // Model the separate request mailbox overtaking the queued withdrawal.
        subscribe_at(&mut session, 9, ClientId(1), u64::MAX, out_tx);
        assert!(
            session
                .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
                .is_empty()
        );
        assert!(matches!(
            detach.await,
            CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                ..
            }
        ));
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"newer attachment")))
            .unwrap();
        assert!(matches!(
            out_rx.try_recv(),
            Ok(Outbound::Frame(FrameKind::TerminalOutput { .. }))
        ));
        assert!(
            session.pending_detaches.is_empty(),
            "newer upstream stream is untouched"
        );
    }

    #[test]
    fn upstream_detach_barrier_discards_old_frames_before_a_fresh_attach() {
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
        );
        session.handle_inbound(&encode(&begin_frame(9, 1))).unwrap();
        session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
        let (events_tx, mut events_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(2), events_tx);
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let (reply, _received) = oneshot::channel();
        let request = RelayRequest::Command {
            command: Command::AttachTerminal {
                terminal_id: TerminalId::local(9),
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 9,
                client: ClientId(1),
                out_tx,
                seq: 1,
                awaits_snapshot: true,
                bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: Some(BootstrapLimits::default()),
            }),
        };
        let prepared = session.prepare_request(&request);
        let [barrier, restore_events] = prepared.as_slice() else {
            panic!("detach and event restoration before attach");
        };
        assert!(matches!(
            decode(restore_events),
            FrameKind::SubscribeEvents {
                terminal: Some(TerminalId::Local { id: 9 })
            }
        ));
        let FrameKind::Command {
            request_id,
            command: Command::DetachTerminal { terminal_id },
        } = decode(barrier)
        else {
            panic!("detach barrier");
        };
        assert_eq!(terminal_id, TerminalId::local(9));
        assert!(
            session.prepare_request(&request).is_empty(),
            "reuse an in-flight barrier"
        );
        session.handle_request_checked(request).unwrap();
        let event = FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: phux_protocol::wire::frame::AgentEvent::CommandStarted,
        };
        session.handle_inbound(&encode(&event)).unwrap();
        assert!(
            matches!(
                events_rx.try_recv(),
                Ok(Outbound::Frame(FrameKind::Event { .. }))
            ),
            "an event observer does not suppress the first content barrier and stays live during it"
        );
        assert!(
            out_rx.try_recv().is_err(),
            "new content proxy still waits for its own prefix"
        );
        // The prefix itself may have been queued before withdrawal. It must
        // not open a new proxy's gate, nor fail validation against the old cut.
        session.handle_inbound(&encode(&begin_frame(9, 2))).unwrap();
        session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"old")))
            .unwrap();
        assert!(out_rx.try_recv().is_err());
        session
            .handle_inbound(&encode(&FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            }))
            .unwrap();
        assert!(session.pending_detaches.is_empty());
        assert!(!session.bootstrap_flows.contains_key(&9));
        session.handle_inbound(&encode(&begin_frame(9, 1))).unwrap();
        session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"fresh")))
            .unwrap();
        assert_eq!(
            out_rx.len(),
            3,
            "only the fresh BEGIN, READY and output reach the proxy"
        );
    }

    #[test]
    fn detach_without_a_proxy_retires_automatic_spawn_output() {
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
        );
        session.handle_inbound(&encode(&begin_frame(9, 1))).unwrap();
        session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
        let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
            client: ClientId(1),
            terminal: 9,
            seq: 1,
            reply: None,
        });
        assert_eq!(
            frames.len(),
            1,
            "unobserved spawn producer still needs upstream detach"
        );
        let FrameKind::Command { request_id, .. } = decode(&frames[0]) else {
            panic!("detach command");
        };
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"queued before detach")))
            .unwrap();
        session
            .handle_inbound(&encode(&FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            }))
            .unwrap();
        assert!(session.pending_detaches.is_empty());
        assert!(session.bootstrap_flows.is_empty());
    }

    #[test]
    fn upstream_detach_barrier_refusal_fails_the_link_closed() {
        for result in [
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: "refused".to_owned(),
            },
            CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::Json(
                "{}".to_owned(),
            )),
        ] {
            let mut session = RelaySession::new(host(), BootstrapLimits::default());
            let frame = session.encode_terminal_detach(9);
            let FrameKind::Command { request_id, .. } = decode(&frame) else {
                panic!("detach command");
            };
            assert!(
                session
                    .handle_inbound(&encode(&FrameKind::CommandResult { request_id, result }))
                    .is_err()
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn upstream_detach_barriers_expire_and_reserve_their_request_ids() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let frame = session.encode_terminal_detach(9);
        let FrameKind::Command { request_id, .. } = decode(&frame) else {
            panic!("detach command");
        };
        session.next_request_id = request_id;
        assert_ne!(
            session.allocate_request_id(),
            request_id,
            "in-flight detach owns its correlation"
        );
        session.check_detach_deadlines().unwrap();
        tokio::time::advance(RELAY_COMMAND_TIMEOUT).await;
        assert!(session.check_detach_deadlines().is_err());
        session.teardown("expired upstream detach");
        assert!(session.pending_detaches.is_empty());
    }

    #[tokio::test]
    async fn subscribe_on_a_full_mailbox_registers_nothing_and_notifies_the_consumer() {
        // phux-v45.11 finding 2: the register + forward pair is atomic —
        // when the request cannot be enqueued the consumer gets a typed
        // error push and no hub-side registration exists.
        let (handle, _mailbox) = RelayHandle::new(host());
        for _ in 0..RELAY_MAILBOX {
            handle.forward(FrameKind::Detach);
        }
        let (out_tx, mut out_rx) = mpsc::channel(8);
        handle.subscribe(
            ProxySubscription {
                terminal: 9,
                client: ClientId(1),
                out_tx,
                // Stamped by `handle.subscribe` at enqueue.
                seq: 0,
                awaits_snapshot: false,
                bootstrap_profile: None,
                bootstrap_limits: None,
            },
            FrameKind::SubscribeEvents {
                terminal: Some(TerminalId::local(9)),
            },
        );
        let Outbound::Frame(frame) = out_rx.try_recv().expect("typed error pushed") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(matches!(
            frame,
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::ResourceExhausted,
                ..
            }
        ));
    }

    #[test]
    fn satellite_error_rolls_back_the_commands_subscription() {
        // phux-v45.11 finding 3: a subscribing command the satellite
        // refuses must not leave a proxy registration behind.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let (reply, mut reply_rx) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::AttachTerminal {
                terminal_id: TerminalId::local(9),
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 9,
                client: ClientId(1),
                out_tx,
                seq: 1,
                // Ungated on purpose: this exercises the phux-v45.11 rollback
                // path in isolation. If rollback regressed, the trailing
                // TERMINAL_OUTPUT must actually leak — a gated subscriber
                // would suppress it and mask the regression.
                awaits_snapshot: false,
                bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: Some(BootstrapLimits::default()),
            }),
        });
        let FrameKind::Command { request_id, .. } = decode(&wire) else {
            panic!("expected COMMAND");
        };
        session
            .handle_inbound(&encode(&FrameKind::CommandResult {
                request_id,
                result: CommandResult::Error {
                    code: ErrorCode::TerminalNotFound,
                    message: "nope".to_owned(),
                },
            }))
            .expect("valid satellite frame");
        assert!(matches!(
            reply_rx.try_recv().expect("resolved"),
            CommandResult::Error { .. }
        ));
        // The rolled-back registration must not fan anything out.
        session
            .handle_inbound(&encode(&FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(9),
                stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"leak"),
            }))
            .expect("valid satellite frame");
        assert!(out_rx.try_recv().is_err(), "rolled-back subscriber leaked");
    }

    #[test]
    fn errored_upgrade_preserves_a_concurrently_retained_snapshot() {
        // phux-v45.16's exact triple-coincidence regression:
        //
        // 1. B's event subscription upgrades to an attach, re-gating its
        //    flowing stream to AwaitingFirst.
        // 2. C attaches to the same terminal; its return-leg snapshot fans
        //    out while B's mailbox is full, so B retains that snapshot.
        // 3. B's upgrade gets a transient error reply. Its Regated rollback
        //    must not replace the newer Retained gate with Open, or the next
        //    delta reaches B without the snapshot (L1 §9.1).
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (tx_b, mut rx_b) = mpsc::channel(2);
        let (tx_c, mut rx_c) = mpsc::channel(8);

        // B's event-only stream is established and demonstrably Open.
        subscribe(&mut session, 9, ClientId(2), tx_b.clone());
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(TerminalId::local(9)),
                event: AgentEvent::CommandStarted,
            }))
            .expect("valid satellite frame");
        assert!(rx_b.try_recv().is_ok(), "B's event stream must be open");

        // B upgrades. Keep its request id so the delayed error can arrive
        // after C's attach snapshot.
        let (reply_b, mut reply_rx_b) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::AttachTerminal {
                terminal_id: TerminalId::local(9),
            },
            reply: reply_b,
            subscribe: Some(ProxySubscription {
                terminal: 9,
                client: ClientId(2),
                out_tx: tx_b.clone(),
                seq: 2,
                awaits_snapshot: true,
                bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: Some(BootstrapLimits::default()),
            }),
        });
        let FrameKind::Command { request_id, .. } = decode(&wire) else {
            panic!("expected B's attach COMMAND");
        };

        // C attaches, then B's mailbox fills before C's snapshot returns.
        // The snapshot lands for C but is retained for B.
        attach(&mut session, 9, ClientId(3), tx_c);
        tx_b.try_send(Outbound::Frame(FrameKind::Detach))
            .expect("B filler one");
        tx_b.try_send(Outbound::Frame(FrameKind::Detach))
            .expect("B filler two");
        session
            .handle_inbound(&encode(&snapshot_frame(9)))
            .expect("valid satellite frame");
        assert!(matches!(
            rx_c.try_recv().expect("C receives its snapshot"),
            Outbound::Frame(FrameKind::BootstrapReady { .. })
        ));

        // The delayed transient error rolls back B's upgrade. It may only
        // undo AwaitingFirst; B's concurrently Retained snapshot must win.
        session
            .handle_inbound(&encode(&FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::InternalError,
                message: "transient".to_owned(),
            }))
            .expect("valid satellite frame");
        assert!(matches!(
            reply_rx_b.try_recv().expect("B's upgrade resolves"),
            CommandResult::Error { .. }
        ));

        // Once B has room, its next delta must flush the retained snapshot
        // first and then follow it. The old unconditional rollback delivered
        // only this delta, proving this assertion is non-vacuous.
        assert!(matches!(
            rx_b.try_recv().expect("B filler one drains"),
            Outbound::Frame(FrameKind::Detach)
        ));
        assert!(matches!(
            rx_b.try_recv().expect("B filler two drains"),
            Outbound::Frame(FrameKind::Detach)
        ));
        session
            .handle_inbound(&encode(&output_frame(9, 1, b"after-error")))
            .expect("valid satellite frame");
        assert!(matches!(
            rx_b.try_recv().expect("B's retained snapshot flushes"),
            Outbound::Frame(FrameKind::BootstrapReady { .. })
        ));
        assert!(matches!(
            rx_b.try_recv().expect("B's delta follows the snapshot"),
            Outbound::Frame(FrameKind::TerminalOutput { seq: 1, .. })
        ));
    }

    #[test]
    fn satellite_error_never_rolls_back_a_preexisting_subscription() {
        // The rollback never removes a registration the failing command did
        // not create: an idempotent re-subscribe that errors must leave the
        // original (successful) subscribe streaming. Here the re-subscribe
        // upgrades an event-only stream to an attach, so it re-gates the
        // stream (phux-v45.15, `Registration::Regated`); the error must
        // *restore* the gate to `Open` rather than strand the pre-existing
        // stream behind a snapshot that a refused attach never sends.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        let (reply, _reply_rx) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::AttachTerminal {
                terminal_id: TerminalId::local(9),
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 9,
                client: ClientId(1),
                out_tx,
                // The UPGRADE: a newer token than the pre-existing event-only
                // registration at 1, now awaiting the attach's snapshot.
                seq: 2,
                awaits_snapshot: true,
                bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: Some(BootstrapLimits::default()),
            }),
        });
        let FrameKind::Command { request_id, .. } = decode(&wire) else {
            panic!("expected COMMAND");
        };
        session
            .handle_inbound(&encode(&FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::InternalError,
                message: "transient".to_owned(),
            }))
            .expect("valid satellite frame");
        session
            .handle_inbound(&encode(&FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(9),
                stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"still here"),
            }))
            .expect("valid satellite frame");
        assert!(
            out_rx.try_recv().is_ok(),
            "pre-existing subscription must survive the errored re-subscribe"
        );
    }

    // --- fail-fast + handle backpressure -----------------------------------

    #[tokio::test]
    async fn fail_fast_resolves_commands_with_satellite_unreachable() {
        let (reply, rx) = oneshot::channel();
        fail_fast(
            RelayRequest::Command {
                command: Command::Upgrade,
                reply,
                subscribe: None,
            },
            &host(),
            "backoff",
        );
        assert!(matches!(
            rx.await.expect("resolved"),
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn handle_command_times_out_against_a_silent_satellite() {
        let (handle, rx) = RelayHandle::new(host());
        // Keep the receiver alive but never drain it: the link looks up
        // (mailbox accepts) yet no reply ever arrives — the silent
        // partition / frame-swallowing satellite shape. Paused time
        // auto-advances past the deadline.
        let result = handle.command(Command::Upgrade).await;
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ref message,
            } if message.contains("did not answer within")
        ));
        drop(rx);
    }

    #[test]
    fn prune_abandoned_drops_only_commands_whose_consumer_gave_up() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply_live, mut live_rx) = oneshot::channel();
        let (reply_gone, gone_rx) = oneshot::channel();
        let _ = session.handle_request(RelayRequest::Command {
            command: Command::Upgrade,
            reply: reply_live,
            subscribe: None,
        });
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::Upgrade,
            reply: reply_gone,
            subscribe: None,
        });
        let FrameKind::Command { request_id, .. } = decode(&wire) else {
            panic!("expected COMMAND");
        };

        assert_eq!(session.prune_abandoned(), 0, "both consumers still wait");

        // The consumer of the second command times out / disconnects.
        drop(gone_rx);
        assert_eq!(session.prune_abandoned(), 1, "abandoned entry pruned");

        // A late reply for the pruned id is dropped without touching the
        // still-live command; the live one still resolves (via teardown).
        session
            .handle_inbound(&encode(&FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            }))
            .expect("valid satellite frame");
        assert!(live_rx.try_recv().is_err(), "live command still pending");
        session.teardown("done");
        assert!(matches!(
            live_rx.try_recv().expect("live command survived pruning"),
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn handle_command_fails_fast_when_mailbox_is_full_or_closed() {
        let (handle, mut mailbox) = RelayHandle::new(host());
        // Fill the bounded mailbox.
        for _ in 0..RELAY_MAILBOX {
            handle.forward(FrameKind::Detach);
        }
        let result = handle.command(Command::Upgrade).await;
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                ..
            }
        ));
        // Drop the receiver: the link task is gone.
        mailbox.requests.close();
        while mailbox.requests.try_recv().is_ok() {}
        drop(mailbox);
        let result = handle.command(Command::Upgrade).await;
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
    }
}
