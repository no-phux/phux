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

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::ids::{GroupId, SatelliteHost, TerminalId};
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
}

/// A subscription-withdrawal request, carried on the relay's dedicated
/// **unbounded** unsubscribe channel (phux-v45.11 finding 1): teardown
/// must never be droppable under mailbox pressure, or a detached
/// consumer's `ProxySubscriber` entry outlives it and every future
/// return-leg frame is `try_send`-ed into a dead mailbox. Unbounded is
/// safe here — at most a handful per consumer disconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    },
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
        /// Group under which the satellite spawns (validated there).
        group: GroupId,
        /// Command + argv, or `None` for the satellite's default shell.
        command: Option<Vec<String>>,
        /// Working directory on the satellite, or `None` for its default.
        cwd: Option<String>,
        /// Environment pairs, `None` = inherit the satellite's env.
        env: Option<Vec<(String, String)>>,
        /// First-class `TERM` override, `None` = the satellite's default.
        term: Option<String>,
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
    pub(crate) async fn spawn(
        &self,
        group: GroupId,
        command: Option<Vec<String>>,
        cwd: Option<String>,
        env: Option<Vec<(String, String)>>,
        term: Option<String>,
    ) -> SpawnResult {
        let (reply, rx) = oneshot::channel();
        match self.tx.try_send(RelayRequest::Spawn {
            group,
            command,
            cwd,
            env,
            term,
            reply,
        }) {
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
    /// channel as [`Self::unsubscribe_client`].
    pub(crate) fn unsubscribe_terminal(&self, client: ClientId, terminal: u32) {
        let seq = self.next_seq();
        let _ = self.unsub_tx.send(Unsubscribe::Terminal {
            client,
            terminal,
            seq,
        });
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
    /// A return-leg `TERMINAL_SNAPSHOT` whose fan-out this consumer's
    /// briefly-full mailbox refused, retained to retry before any later
    /// delta reaches it (phux-v45.12). While retained, deltas to this
    /// subscriber are suppressed so a delta can never overtake the dropped
    /// snapshot; the retained frame is retried on the next inbound frame for
    /// the terminal and on the link keepalive tick
    /// ([`RelaySession::flush_pending_snapshots`]), and a newer snapshot (a
    /// satellite resync) replaces it — full-grid, the freshest wins.
    Retained(FrameKind),
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
#[derive(Debug)]
pub(crate) struct RelaySession {
    host: SatelliteHost,
    bootstrap_limits: BootstrapLimits,
    next_request_id: u32,
    pending: HashMap<u32, PendingCommand>,
    /// Relayed `SPAWN_TERMINAL`s awaiting their `TERMINAL_SPAWNED`
    /// (phux-v45.6). Shares the link-side `request_id` space with
    /// [`Self::pending`] so one allocator covers both reply frames.
    pending_spawns: HashMap<u32, oneshot::Sender<SpawnResult>>,
    subscribers: HashMap<u32, Vec<ProxySubscriber>>,
    encode_buf: BytesMut,
}

impl RelaySession {
    /// Fresh session state for one established connection to `host`.
    pub(crate) fn new(host: SatelliteHost, bootstrap_limits: BootstrapLimits) -> Self {
        Self {
            host,
            bootstrap_limits,
            next_request_id: 1,
            pending: HashMap::new(),
            pending_spawns: HashMap::new(),
            subscribers: HashMap::new(),
            encode_buf: BytesMut::with_capacity(1024),
        }
    }

    /// Service one consumer request. Returns the encoded frame to put on
    /// the wire (every request produces one — subscription registration
    /// happens as a side effect of its carrying request, phux-v45.11).
    ///
    /// Split from the wire write so the caller (the link supervisor's
    /// select loop) owns all connection I/O in one place.
    pub(crate) fn handle_request(&mut self, request: RelayRequest) -> Vec<u8> {
        match request {
            RelayRequest::Command {
                command,
                reply,
                subscribe,
            } => {
                // Register the subscription rider in the same step as the
                // command enqueue (phux-v45.11 finding 2), remembering
                // enough to roll it back on an error reply (finding 3).
                let subscription = subscribe.map(|sub| {
                    let (terminal, client) = (sub.terminal, sub.client);
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
                self.encode(&FrameKind::Command {
                    request_id,
                    command,
                })
            }
            RelayRequest::Forward { frame } => self.encode(&frame),
            RelayRequest::Spawn {
                group,
                command,
                cwd,
                env,
                term,
                reply,
            } => {
                let request_id = self.allocate_request_id();
                self.pending_spawns.insert(request_id, reply);
                self.encode(&FrameKind::SpawnTerminal {
                    request_id,
                    group,
                    command,
                    cwd,
                    env,
                    term,
                    // The satellite spawns locally: the addressing field
                    // never crosses the link (hub-and-spoke, no chaining).
                    satellite: None,
                    owner_terminal: None,
                    agent_session: None,
                })
            }
            RelayRequest::Subscribe {
                subscription,
                forward,
            } => {
                // Atomic with the wire write: the caller either sees this
                // request accepted (registration + forward both happen —
                // the supervisor writes the returned frame) or refused
                // up-front with a typed error and no registration.
                self.register_subscriber(subscription);
                self.encode(&forward)
            }
        }
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
    /// terminal whose **last** proxy subscriber just went away, so the
    /// satellite stops streaming output for terminals nobody on this hub
    /// observes anymore. Fire-and-forget: the link allocates a request id
    /// but registers no pending entry — the reply (always `Ok`;
    /// `DETACH_TERMINAL` is idempotent) is logged and dropped.
    pub(crate) fn handle_unsubscribe(&mut self, unsubscribe: Unsubscribe) -> Vec<Vec<u8>> {
        let mut orphaned: Vec<u32> = Vec::new();
        match unsubscribe {
            Unsubscribe::Client(client) => {
                self.subscribers.retain(|terminal, subs| {
                    subs.retain(|s| s.client != client);
                    if subs.is_empty() {
                        orphaned.push(*terminal);
                        false
                    } else {
                        true
                    }
                });
            }
            Unsubscribe::Terminal {
                client,
                terminal,
                seq,
            } => {
                if let Some(subs) = self.subscribers.get_mut(&terminal) {
                    // Drop this withdrawal if a registration for the same
                    // client with a token at least as new exists: a
                    // detach-then-reattach can arrive here reordered (the
                    // re-attach rode the bounded request mailbox, this
                    // detach the unbounded unsubscribe channel), and
                    // applying the stale detach would tear the live
                    // re-attach down and emit a spurious satellite-side
                    // DETACH_TERMINAL (phux-v45.7).
                    let superseded = subs.iter().any(|s| s.client == client && s.seq >= seq);
                    if superseded {
                        debug!(
                            satellite = %self.host,
                            terminal,
                            ?client,
                            "stale terminal unsubscribe superseded by a newer re-attach; dropping"
                        );
                    } else {
                        subs.retain(|s| s.client != client);
                        if subs.is_empty() {
                            self.subscribers.remove(&terminal);
                            orphaned.push(terminal);
                        }
                    }
                }
            }
        }
        orphaned
            .into_iter()
            .map(|terminal| {
                debug!(
                    satellite = %self.host,
                    terminal,
                    "last proxy subscriber gone; detaching satellite-side"
                );
                let request_id = self.allocate_request_id();
                self.encode(&FrameKind::Command {
                    request_id,
                    command: Command::DetachTerminal {
                        terminal_id: TerminalId::local(terminal),
                    },
                })
            })
            .collect()
    }

    /// Dispatch one frame arriving from the satellite: resolve relayed
    /// command and spawn replies and re-tag + fan out subscribed streams.
    #[allow(
        clippy::too_many_lines,
        reason = "one resolve/re-tag arm per relayable return-leg frame kind; splitting hides the catalog"
    )]
    pub(crate) fn handle_inbound(&mut self, framed: &[u8]) -> Result<(), String> {
        let frame = FrameKind::decode_with_limits(framed, self.bootstrap_limits)
            .map_err(|err| {
                format!(
                    "satellite {} sent an undecodable frame: {err:?}",
                    self.host
                )
            })?
            .0;
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
            } => {
                // Correlated errors resolve whichever request kind holds
                // the id — commands own it in the common case, but a
                // satellite MAY answer a relayed spawn with a generic
                // correlated ERROR instead of TERMINAL_SPAWNED.
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
            FrameKind::Event { terminal, event } => {
                if let Some(id) = self.retag_inbound(terminal.as_ref()) {
                    self.fan_out(
                        id,
                        &FrameKind::Event {
                            terminal: Some(TerminalId::satellite(self.host.clone(), id)),
                            event,
                        },
                    );
                }
            }
            FrameKind::TerminalOutput {
                terminal_id,
                seq,
                bytes,
            } => {
                if let Some(id) = self.retag_inbound(Some(&terminal_id)) {
                    self.fan_out(
                        id,
                        &FrameKind::TerminalOutput {
                            terminal_id: TerminalId::satellite(self.host.clone(), id),
                            seq,
                            bytes,
                        },
                    );
                }
            }
            FrameKind::TerminalSnapshot {
                terminal_id,
                cols,
                rows,
                vt_replay_bytes,
                scrollback_bytes,
            } => {
                if let Some(id) = self.retag_inbound(Some(&terminal_id)) {
                    self.fan_out(
                        id,
                        &FrameKind::TerminalSnapshot {
                            terminal_id: TerminalId::satellite(self.host.clone(), id),
                            cols,
                            rows,
                            vt_replay_bytes,
                            scrollback_bytes,
                        },
                    );
                }
            }
            FrameKind::TerminalClosed {
                terminal_id,
                exit_status,
            } => {
                if let Some(id) = self.retag_inbound(Some(&terminal_id)) {
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
                        },
                    );
                    // The satellite terminal is gone; its proxy
                    // subscriptions go with it.
                    self.subscribers.remove(&id);
                }
            }
            FrameKind::Bell { terminal_id } => {
                if let Some(id) = self.retag_inbound(Some(&terminal_id)) {
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
            }
            other => {
                return Err(format!(
                    "satellite {} sent a direction-invalid frame after HELLO_OK: {other:?}",
                    self.host
                ));
            }
        }
        Ok(())
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
        let is_snapshot = matches!(frame, FrameKind::TerminalSnapshot { .. });
        for sub in subs.iter_mut() {
            if is_snapshot {
                match sub.out_tx.try_send(Outbound::Frame(frame.clone())) {
                    // Delivered: the gate opens and deltas may flow (this is
                    // both the attach subscriber's first snapshot clearing
                    // `AwaitingFirst` and a retained snapshot landing).
                    Ok(()) => sub.gate = SnapshotGate::Open,
                    // Retain it: a later delta must not overtake it.
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        sub.gate = SnapshotGate::Retained(frame.clone());
                    }
                    // Dead consumer; teardown / disconnect reaps it.
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                }
            } else if Self::flush_pending_snapshot(sub) {
                // The gate is open (its snapshot has landed) — the delta may
                // ride. `flush_pending_snapshot` also retries a `Retained`
                // snapshot first so the delta only follows once it lands.
                let _ = sub.out_tx.try_send(Outbound::Frame(frame.clone()));
            }
            // else: the subscriber is still `AwaitingFirst` (its snapshot has
            // not been delivered) or its `Retained` snapshot is stuck behind
            // a full mailbox — the delta is dropped now, since delivering it
            // would precede the snapshot (L1 §9.1).
        }
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
    /// fire-and-forget: a full mailbox still drops it (genuinely best-effort,
    /// the same discipline as every other delta).
    fn fan_out_ungated(&self, id: u32, frame: &FrameKind) {
        let Some(subs) = self.subscribers.get(&id) else {
            trace!(satellite = %self.host, terminal = id, "ungated frame with no proxy subscribers");
            return;
        };
        for sub in subs {
            let _ = sub.out_tx.try_send(Outbound::Frame(frame.clone()));
        }
    }

    /// Retry one subscriber's retained attach snapshot (phux-v45.12).
    /// Returns `true` when the gate is [`SnapshotGate::Open`] or the retained
    /// snapshot was just delivered (the subscriber may receive deltas),
    /// `false` while the subscriber is [`SnapshotGate::AwaitingFirst`]
    /// (phux-v45.14, no snapshot delivered yet) or its retained snapshot
    /// stays stuck behind a full mailbox. A closed mailbox opens the gate
    /// (the dead consumer is reaped elsewhere) and reads as delivered so
    /// callers do not loop.
    fn flush_pending_snapshot(sub: &mut ProxySubscriber) -> bool {
        match &sub.gate {
            SnapshotGate::Open => true,
            SnapshotGate::AwaitingFirst => false,
            SnapshotGate::Retained(snapshot) => {
                match sub.out_tx.try_send(Outbound::Frame(snapshot.clone())) {
                    // Delivered, or the consumer is gone (reaped elsewhere):
                    // either way the gate opens and deltas may resume.
                    Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {
                        sub.gate = SnapshotGate::Open;
                        true
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => false,
                }
            }
        }
    }

    /// Retry every subscriber's retained attach snapshot (phux-v45.12).
    /// Driven from the link supervisor's keepalive tick so a consumer whose
    /// mailbox was briefly full at attach still converges even if no further
    /// return-leg frame arrives for its terminal to trigger the inline retry
    /// in [`Self::fan_out`].
    pub(crate) fn flush_pending_snapshots(&mut self) {
        for subs in self.subscribers.values_mut() {
            for sub in subs.iter_mut() {
                if matches!(sub.gate, SnapshotGate::Retained(_)) {
                    let _ = Self::flush_pending_snapshot(sub);
                }
            }
        }
    }

    /// Allocate the next link-side request id, skipping ids still pending
    /// in either reply map (u32 wrap-around safety, not a practical
    /// collision).
    fn allocate_request_id(&mut self) -> u32 {
        loop {
            let id = self.next_request_id;
            self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
            if !self.pending.contains_key(&id) && !self.pending_spawns.contains_key(&id) {
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
                terminal_id: sat,
                id: "q".to_owned(),
                question: "?".to_owned(),
                suggestions: vec![],
                elapsed_seconds: None,
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
        session.handle_inbound(&encode(&FrameKind::CommandResult {
            request_id: id_b,
            result: CommandResult::OkWith(CommandValue::Json("b".to_owned())),
        })).expect("valid satellite frame");
        session.handle_inbound(&encode(&FrameKind::CommandResult {
            request_id: id_a,
            result: CommandResult::Ok,
        })).expect("valid satellite frame");

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
        session.handle_inbound(&encode(&FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::TerminalNotFound,
            message: "nope".to_owned(),
        })).expect("valid satellite frame");
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
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
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
        session.handle_inbound(&encode(&FrameKind::TerminalSpawned {
            request_id,
            result: SpawnResult::Ok(TerminalId::local(42)),
        })).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&FrameKind::TerminalSpawned {
            request_id,
            result: SpawnResult::Ok(TerminalId::satellite("nested", 7)),
        })).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&FrameKind::TerminalSpawned {
            request_id,
            result: SpawnResult::Err(SpawnError::GroupNotFound),
        })).expect("valid satellite frame");
        assert_eq!(
            rx.try_recv().expect("resolved"),
            SpawnResult::Err(SpawnError::GroupNotFound)
        );
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

        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
        session.handle_inbound(&encode(&FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(9),
            seq: 42,
            bytes: bytes::Bytes::from_static(b"hi"),
        })).expect("valid satellite frame");
        // A different terminal: nothing must reach the subscriber.
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(10)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");

        let Outbound::Frame(first) = out_rx.try_recv().expect("event fanned out");
        assert_eq!(
            first,
            FrameKind::Event {
                terminal: Some(TerminalId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
            }
        );
        let Outbound::Frame(second) = out_rx.try_recv().expect("output fanned out");
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
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::satellite("nested", 9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
        assert!(out_rx.try_recv().is_err());
    }

    #[test]
    fn terminal_closed_retags_and_drops_the_proxy_subscription() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        session.handle_inbound(&encode(&FrameKind::TerminalClosed {
            terminal_id: TerminalId::local(9),
            exit_status: Some(0),
        })).expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("closed fanned out");
        assert_eq!(
            frame,
            FrameKind::TerminalClosed {
                terminal_id: TerminalId::satellite("devbox", 9),
                exit_status: Some(0),
            }
        );
        // Subscription is gone: further frames for id 9 do not fan out.
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
        assert!(out_rx.try_recv().is_err());
    }

    // --- attach snapshot ordering under backpressure (phux-v45.12) --------

    fn snapshot_frame(id: u32) -> FrameKind {
        FrameKind::TerminalSnapshot {
            terminal_id: TerminalId::local(id),
            cols: 80,
            rows: 24,
            vt_replay_bytes: b"grid".to_vec(),
            scrollback_bytes: None,
        }
    }

    fn output_frame(id: u32, seq: u64, bytes: &'static [u8]) -> FrameKind {
        FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(id),
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
        session.handle_inbound(&encode(&snapshot_frame(9))).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&output_frame(9, 1, b"delta"))).expect("valid satellite frame");

        let Outbound::Frame(first) = out_rx.try_recv().expect("snapshot delivered");
        assert!(
            matches!(first, FrameKind::TerminalSnapshot { .. }),
            "the snapshot must reach the consumer before any delta, got {first:?}"
        );
        let Outbound::Frame(second) = out_rx.try_recv().expect("delta delivered");
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
        session.handle_inbound(&encode(&snapshot_frame(9))).expect("valid satellite frame");
        session.handle_inbound(&encode(&output_frame(9, 1, b"delta"))).expect("valid satellite frame");

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
        let Outbound::Frame(frame) = out_rx.try_recv().expect("snapshot flushed on tick");
        assert!(
            matches!(frame, FrameKind::TerminalSnapshot { .. }),
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
        session.handle_inbound(&encode(&FrameKind::TerminalSnapshot {
            terminal_id: TerminalId::local(9),
            cols: 80,
            rows: 24,
            vt_replay_bytes: b"stale".to_vec(),
            scrollback_bytes: None,
        })).expect("valid satellite frame");
        // A fresher snapshot arrives (still full) and must replace it.
        session.handle_inbound(&encode(&FrameKind::TerminalSnapshot {
            terminal_id: TerminalId::local(9),
            cols: 80,
            rows: 24,
            vt_replay_bytes: b"fresh".to_vec(),
            scrollback_bytes: None,
        })).expect("valid satellite frame");
        assert!(matches!(
            out_rx.try_recv().expect("filler drains"),
            Outbound::Frame(FrameKind::Detach)
        ));
        session.flush_pending_snapshots();
        let Outbound::Frame(FrameKind::TerminalSnapshot {
            vt_replay_bytes, ..
        }) = out_rx.try_recv().expect("snapshot flushed")
        else {
            panic!("expected a snapshot");
        };
        assert_eq!(
            vt_replay_bytes, b"fresh",
            "the freshest retained snapshot must win"
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
        session.handle_inbound(&encode(&snapshot_frame(9))).expect("valid satellite frame");
        let Outbound::Frame(a_snap) = rx_a.try_recv().expect("A's snapshot");
        assert!(matches!(a_snap, FrameKind::TerminalSnapshot { .. }));

        // B attaches to the same terminal (registration is immediate) but its
        // snapshot has not been requested/answered yet.
        attach(&mut session, 9, ClientId(2), tx_b);

        // A's stream produces a delta before B's snapshot arrives. It fans
        // out to both subscribers — A (Open) receives it; B (AwaitingFirst)
        // must have it suppressed.
        session.handle_inbound(&encode(&output_frame(9, 1, b"a-stream"))).expect("valid satellite frame");
        let Outbound::Frame(a_delta) = rx_a.try_recv().expect("A sees the delta");
        assert!(matches!(a_delta, FrameKind::TerminalOutput { seq: 1, .. }));
        assert!(
            rx_b.try_recv().is_err(),
            "B must not see a delta before its own snapshot (L1 §9.1)"
        );

        // B's own snapshot finally lands: it is delivered, opening B's gate.
        session.handle_inbound(&encode(&snapshot_frame(9))).expect("valid satellite frame");
        let Outbound::Frame(b_first) = rx_b.try_recv().expect("B's snapshot lands");
        assert!(
            matches!(b_first, FrameKind::TerminalSnapshot { .. }),
            "B's first frame must be its snapshot, got {b_first:?}"
        );

        // A subsequent delta now rides after B's snapshot, in order.
        session.handle_inbound(&encode(&output_frame(9, 2, b"after"))).expect("valid satellite frame");
        let Outbound::Frame(b_delta) = rx_b.try_recv().expect("B sees the post-snapshot delta");
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
        session.handle_inbound(&encode(&output_frame(9, 1, b"suppressed"))).expect("valid satellite frame");
        assert!(
            rx_b.try_recv().is_err(),
            "a content delta is suppressed before the snapshot"
        );

        // ...but the terminal closing must reach B before it is reaped.
        session.handle_inbound(&encode(&FrameKind::TerminalClosed {
            terminal_id: TerminalId::local(9),
            exit_status: Some(0),
        })).expect("valid satellite frame");
        let Outbound::Frame(frame) = rx_b.try_recv().expect("close delivered past the gate");
        assert_eq!(
            frame,
            FrameKind::TerminalClosed {
                terminal_id: TerminalId::satellite("devbox", 9),
                exit_status: Some(0),
            },
            "a gated subscriber must still see TERMINAL_CLOSED"
        );
        // And the subscription is reaped: no further fan-out for the id.
        session.handle_inbound(&encode(&output_frame(9, 2, b"after-close"))).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("event flows without a snapshot");
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
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
        assert!(
            out_rx.try_recv().is_ok(),
            "the event-only stream must be flowing before the upgrade"
        );

        // 2. UPGRADE the same client to an attach on the same terminal.
        attach(&mut session, 9, ClientId(1), out_tx);

        // 3. A content delta arrives before the attach's snapshot. It must now
        //    be suppressed — the upgrade re-gated the stream.
        session.handle_inbound(&encode(&output_frame(9, 1, b"pre-snapshot"))).expect("valid satellite frame");
        assert!(
            out_rx.try_recv().is_err(),
            "the upgrade must gate deltas until its own snapshot (L1 §9.1)"
        );

        // 4. The attach's snapshot lands: delivered, re-opening the gate.
        session.handle_inbound(&encode(&snapshot_frame(9))).expect("valid satellite frame");
        let Outbound::Frame(first) = out_rx.try_recv().expect("the attach snapshot lands");
        assert!(
            matches!(first, FrameKind::TerminalSnapshot { .. }),
            "the first post-upgrade frame must be the snapshot, got {first:?}"
        );

        // 5. A subsequent delta now rides after the snapshot, in order.
        session.handle_inbound(&encode(&output_frame(9, 2, b"post-snapshot"))).expect("valid satellite frame");
        let Outbound::Frame(delta) = out_rx.try_recv().expect("post-snapshot delta rides");
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
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&output_frame(9, 1, b"suppressed"))).expect("valid satellite frame");
        assert!(
            rx.try_recv().is_err(),
            "a content delta is suppressed before the snapshot"
        );

        // ...but a bell rings through, re-tagged, past the gate.
        session.handle_inbound(&encode(&FrameKind::Bell {
            terminal_id: TerminalId::local(9),
        })).expect("valid satellite frame");
        let Outbound::Frame(frame) = rx.try_recv().expect("bell delivered past the gate");
        assert_eq!(
            frame,
            FrameKind::Bell {
                terminal_id: TerminalId::satellite("devbox", 9),
            },
            "a gated subscriber must still see a BELL"
        );

        // The gate is still closed: a further delta stays suppressed until the
        // snapshot lands (the bell bypass does not open the gate).
        session.handle_inbound(&encode(&output_frame(9, 2, b"still-gated"))).expect("valid satellite frame");
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
        let Outbound::Frame(frame) = out_rx.try_recv().expect("consumer notified");
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
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
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
        });
        assert!(
            frames.is_empty(),
            "satellite-side detach must wait for the last subscriber"
        );
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
        let Outbound::Frame(_) = rx_b.try_recv().expect("client 2 still fanned out");
        // Now the last subscriber leaves: exactly one detach goes out.
        let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
            client: ClientId(2),
            terminal: 9,
            seq: 2,
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
        });
        assert!(
            frames.is_empty(),
            "a stale detach must not emit a satellite-side DETACH for a re-attached terminal"
        );

        // The re-attached stream is intact.
        session.handle_inbound(&encode(&FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(9),
            seq: 7,
            bytes: bytes::Bytes::from_static(b"live"),
        })).expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("re-attached stream torn down");
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
        handle.unsubscribe_terminal(ClientId(2), 7);
        assert_eq!(
            mailbox.unsubscribes.try_recv().expect("delivered"),
            Unsubscribe::Client(ClientId(1))
        );
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
            },
            FrameKind::SubscribeEvents {
                terminal: Some(TerminalId::local(9)),
            },
        );
        let Outbound::Frame(frame) = out_rx.try_recv().expect("typed error pushed");
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
            }),
        });
        let FrameKind::Command { request_id, .. } = decode(&wire) else {
            panic!("expected COMMAND");
        };
        session.handle_inbound(&encode(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "nope".to_owned(),
            },
        })).expect("valid satellite frame");
        assert!(matches!(
            reply_rx.try_recv().expect("resolved"),
            CommandResult::Error { .. }
        ));
        // The rolled-back registration must not fan anything out.
        session.handle_inbound(&encode(&FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(9),
            seq: 1,
            bytes: bytes::Bytes::from_static(b"leak"),
        })).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(TerminalId::local(9)),
            event: AgentEvent::CommandStarted,
        })).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&snapshot_frame(9))).expect("valid satellite frame");
        assert!(matches!(
            rx_c.try_recv().expect("C receives its snapshot"),
            Outbound::Frame(FrameKind::TerminalSnapshot { .. })
        ));

        // The delayed transient error rolls back B's upgrade. It may only
        // undo AwaitingFirst; B's concurrently Retained snapshot must win.
        session.handle_inbound(&encode(&FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::InternalError,
            message: "transient".to_owned(),
        })).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&output_frame(9, 1, b"after-error"))).expect("valid satellite frame");
        assert!(matches!(
            rx_b.try_recv().expect("B's retained snapshot flushes"),
            Outbound::Frame(FrameKind::TerminalSnapshot { .. })
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
            }),
        });
        let FrameKind::Command { request_id, .. } = decode(&wire) else {
            panic!("expected COMMAND");
        };
        session.handle_inbound(&encode(&FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::InternalError,
            message: "transient".to_owned(),
        })).expect("valid satellite frame");
        session.handle_inbound(&encode(&FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(9),
            seq: 1,
            bytes: bytes::Bytes::from_static(b"still here"),
        })).expect("valid satellite frame");
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
        session.handle_inbound(&encode(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })).expect("valid satellite frame");
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
