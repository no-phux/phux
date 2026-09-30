//! Hub-to-satellite frame relay (ADR-0007 §4).
//!
//! A frame addressed to `ResourceId::Satellite { host, id }` is rewritten to
//! `Local { id }` and forwarded verbatim over the link for `host`; replies
//! and streams coming back are re-tagged `Satellite { host, id }`. Frames
//! already tagged `Satellite` from a satellite are dropped, never chained.
//!
//! One `RelaySession` lives in each connected link supervisor
//! (`super::link::run_link`) and owns the link-side `request_id` remap and
//! the proxy-subscription registry. Consumers reach it through a
//! `RelayHandle` (a bounded mailbox published in `HubRelays`).
//!
//! **Fail fast, never hang.** Without a live connection every request gets
//! a typed `SatelliteUnreachable` at once; a disconnect fails in-flight
//! commands and notifies every proxy subscriber. A silently dead satellite
//! is bounded by `RELAY_COMMAND_TIMEOUT` and the link keepalive, and
//! abandoned pending entries are pruned on the keepalive tick.
//!
//! **Backpressure.** Every producer uses `try_send` into bounded mailboxes;
//! a saturated link fails commands with `ResourceExhausted`. The one
//! exception is the attach snapshot (L1 §9.1): a snapshot a full consumer
//! refuses is retained per subscriber and its later deltas are held until
//! it lands, so a delta never overtakes it. No link-wide stall.
//!
//! **Directory listings** (`docs/spec/L3.md` §4.1) ride the link as
//! correlated requests; every failure is a typed refusal naming the host.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerFeature, ServerFeatureExt,
    ServerFeatureExtSet, ServerFeatureSet,
};
use phux_protocol::ids::{BootstrapId, GroupId, ResourceId, SatelliteHost, StreamId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ControlAction, DirectoryErrorCode, DirectoryListingError,
    DirectoryListingResult, ErrorCode, FrameKind, HistoryTombstoneReason, PathErrorCode,
    PathQueryError, PathQueryResult, Scope, SpawnError, SpawnResult, TombstoneReason,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::state::{ClientId, Outbound};

/// Capacity of each per-satellite relay mailbox; the link is one ordered
/// stream, so deeper queues only add latency.
pub(crate) const RELAY_MAILBOX: usize = 64;
/// Relay retention limits: per generation 16 MiB / 128 frames, per slow
/// subscriber 2 MiB / 16 frames, per satellite connection 8 MiB / 64 frames.
/// Independent of the negotiated frame ceiling so one hostile generation
/// cannot commit gigabytes.
const MAX_RELAY_GENERATION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RELAY_GENERATION_FRAMES: u32 = 128;
const MAX_RELAY_SUBSCRIBER_RETAINED_BYTES: usize = 2 * 1024 * 1024;
const MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES: usize = 16;
const MAX_RELAY_CONNECTION_RETAINED_BYTES: usize = 8 * 1024 * 1024;
const MAX_RELAY_CONNECTION_RETAINED_FRAMES: usize = 64;
const RETAINED_FRAME_OVERHEAD: usize = 256;
const RELAY_RETIREMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Upper bound on one relayed command round trip. Equal to the transport
/// idle timeout: a loud link failure resolves sooner through teardown, so
/// this catches quiet partitions and satellites that never answer.
pub(crate) const RELAY_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Upper bound on one relayed `LIST_DIRECTORY`: the satellite's own 5 s walk
/// bound plus link time. Shorter than [`RELAY_COMMAND_TIMEOUT`] because a
/// person is waiting on the picker.
pub(crate) const RELAY_LIST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// A `DIRECTORY_LISTING` refusal for `path` (code `Other`, message naming
/// the host).
pub(crate) fn listing_refusal(path: &str, message: impl Into<String>) -> DirectoryListingError {
    DirectoryListingError {
        path: path.to_owned(),
        code: DirectoryErrorCode::Other,
        message: message.into(),
    }
}

/// Relay failures preserve the attempted root and name the satellite.
pub(crate) fn path_refusal(root: &str, message: impl Into<String>) -> PathQueryError {
    PathQueryError {
        root: root.to_owned(),
        code: PathErrorCode::Other,
        message: message.into(),
    }
}

/// One hub consumer's registration on a link's return leg.
#[derive(Debug)]
pub(crate) struct ProxySubscription {
    /// Satellite-local terminal id (the `id` of `Satellite { host, id }`).
    pub(crate) terminal: u32,
    /// Hub-side client identity, for teardown on detach.
    pub(crate) client: ClientId,
    /// The client's outbound mailbox.
    pub(crate) out_tx: mpsc::Sender<Outbound>,
    /// Cancels the owning client connection when failure delivery cannot
    /// progress (other writer clones keep the mailbox alive).
    pub(crate) consumer_cancel: CancellationToken,
    /// Issue-order token from [`RelayHandle`]. A registration and its later
    /// withdrawal ride different channels that may drain in either order;
    /// the token keeps a stale detach from tearing down a newer re-attach.
    pub(crate) seq: u64,
    /// Whether this is a content stream (a relayed `ATTACH_RESOURCE`) that
    /// opens with its own snapshot. Such a subscriber starts gated until that
    /// snapshot lands (L1 §9.1). Event-only subscriptions carry no snapshot
    /// and must flow at once.
    pub(crate) awaits_snapshot: bool,
    /// The downstream connection's bootstrap selection. Content
    /// subscriptions must match the link exactly; event-only may be `None`.
    pub(crate) bootstrap_profile: Option<BootstrapProfile>,
    /// Exact per-frame bounds selected by the downstream connection.
    pub(crate) bootstrap_limits: Option<BootstrapLimits>,
}

/// A subscription withdrawal, on the relay's unbounded unsubscribe channel:
/// teardown must never be dropped under mailbox pressure, or a dead
/// consumer's entry outlives it.
#[derive(Debug)]
pub(crate) enum Unsubscribe {
    /// Drop every proxy subscription `ClientId` holds on this link.
    Client(ClientId),
    /// Drop one client's subscription to one terminal (relayed `DETACH_RESOURCE`).
    Terminal {
        /// The unsubscribing hub-side client.
        client: ClientId,
        /// The satellite-local terminal id it stops observing.
        terminal: u32,
        /// Issue-order token (see [`ProxySubscription::seq`]); ignored if a
        /// newer registration exists.
        seq: u64,
        /// Resolves after the link removes the proxy; `None` from drains that
        /// have no registry.
        reply: Option<WithdrawalReceipt>,
    },
}

/// Serializes a timeout's cancellation with the link's synchronous mutation;
/// a completed result wins. No lock is held across an await.
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

/// Validated spawn payload for one satellite.
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
    /// Kind, parent, and agent-session provenance, parent already reduced to
    /// the satellite's `Local` space (ADR-0104 §6). `None` for a plain spawn.
    pub(crate) resource: Option<Box<phux_protocol::wire::frame::SpawnResource>>,
}

/// A request from a hub-side consumer path to one satellite's relay.
#[derive(Debug)]
pub(crate) enum RelayRequest {
    /// Relay a `COMMAND` (ids already satellite-local) and resolve `reply`
    /// with the correlated result or a typed error.
    Command {
        /// The command to forward, ids already satellite-local.
        command: Command,
        /// Dropping the receiver is legal (detached relays do).
        reply: oneshot::Sender<CommandResult>,
        /// A proxy subscription registered atomically with the enqueue and
        /// rolled back if the satellite answers with an error.
        subscribe: Option<ProxySubscription>,
    },
    /// Relay a fire-and-forget frame (`INPUT_*`, `FRAME_ACK`,
    /// `RESIZE_TERMINAL`); a dead link drops it.
    Forward {
        /// The frame to forward verbatim.
        frame: FrameKind,
    },
    /// Relay a `SPAWN_RESOURCE` (L1 §3.1). The wire frame carries
    /// `satellite: None`; the reply's new id is re-tagged to this host.
    Spawn {
        /// Spawn fields, with owner addressing already validated.
        spawn: SatelliteSpawn,
        /// Resolved with the re-tagged spawn result or a typed error.
        reply: oneshot::Sender<SpawnResult>,
    },
    /// Register a proxy subscription and forward `forward` in one step (the
    /// replyless satellite `SUBSCRIBE_EVENTS`). Idempotent per
    /// `(terminal, client)`.
    Subscribe {
        /// The consumer registration.
        subscription: ProxySubscription,
        /// The frame to forward (already satellite-local).
        forward: FrameKind,
    },
    /// Relay a `LIST_DIRECTORY` (`docs/spec/L3.md` §4.1); `reply` resolves
    /// with the listing or a refusal naming the host.
    ListDirectory {
        /// The requested path, verbatim.
        path: String,
        /// Resolved with the listing or a typed refusal.
        reply: oneshot::Sender<DirectoryListingResult>,
    },
    /// A satellite-local path query; the session strips the hub host field
    /// and correlates `PATH_RESULTS` through a link-side request id.
    #[allow(
        dead_code,
        reason = "consumer dispatch is implemented on the sibling branch"
    )]
    PathQuery {
        root: String,
        query: String,
        recursive: bool,
        reply: oneshot::Sender<PathQueryResult>,
    },
    /// Relay a keyed `COMMAND` (`APPLY_INPUT`, or a kill/signal with an
    /// `operation_id`) from consumer `actor` (L1 §9.1): forwarded only to a
    /// satellite that evaluates it, never across a restart.
    Keyed {
        /// The command to forward, ids already satellite-local.
        command: Command,
        /// The hub consumer that sent it.
        actor: ClientId,
        /// Resolved with the satellite's result or the hub's refusal.
        reply: oneshot::Sender<CommandResult>,
    },
    /// Subscribe and read the agent-metadata allowlist for one terminal
    /// (ADR-0136), once per connection.
    MirrorTerminal {
        /// Satellite-local terminal id.
        terminal: u32,
    },
}

/// Receiving half of one satellite's relay, drained by
/// [`super::link::run_link`].
#[derive(Debug)]
pub(crate) struct RelayMailbox {
    /// Bounded consumer-request mailbox ([`RELAY_MAILBOX`]).
    pub(crate) requests: mpsc::Receiver<RelayRequest>,
    /// Unbounded, undroppable subscription teardown (phux-v45.11).
    pub(crate) unsubscribes: mpsc::UnboundedReceiver<Unsubscribe>,
    /// The hub state whose journal re-stamps relayed events (ADR-0123);
    /// `None` forwards the satellite's stamp.
    pub(crate) journal: Option<crate::state::SharedState>,
    /// This satellite's incarnation fence (L1 §9.1); survives reconnects.
    pub(crate) operations: super::operation_fence::OperationFence,
}

/// The `dropped` count of a satellite's link-level `journal_gap`.
const fn link_journal_gap(frame: &FrameKind) -> Option<u64> {
    match frame {
        FrameKind::Event {
            terminal: None,
            event:
                AgentEvent::JournalGap {
                    first_missing,
                    last_missing,
                },
            ..
        } => Some(
            last_missing
                .saturating_sub(*first_missing)
                .saturating_add(1),
        ),
        _ => None,
    }
}

/// Rewrite a satellite's `journal_gap` as the hub's `source_gap`: events for
/// that terminal were lost before the hub could journal them (ADR-0123).
fn satellite_gap_as_source_gap(event: &mut AgentEvent) {
    if let AgentEvent::JournalGap {
        first_missing,
        last_missing,
    } = *event
    {
        let dropped = last_missing.saturating_sub(first_missing).saturating_add(1);
        *event = AgentEvent::SourceGap { dropped };
    }
}

/// Cheaply-cloneable producer handle to one satellite's relay mailbox.
#[derive(Debug, Clone)]
pub(crate) struct RelayHandle {
    host: SatelliteHost,
    tx: mpsc::Sender<RelayRequest>,
    unsub_tx: mpsc::UnboundedSender<Unsubscribe>,
    /// Issue-order token source shared by every handle for this host, so
    /// all of its subscribe/detach operations are totally ordered.
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
                journal: None,
                operations: super::operation_fence::OperationFence::default(),
            },
        )
    }

    /// Next issue-order token. `Relaxed` suffices on the single-threaded hub.
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// The satellite host this handle relays to.
    pub(crate) const fn host(&self) -> &SatelliteHost {
        &self.host
    }

    /// Relay `command` and await the result. Fails fast with a typed error on
    /// a full mailbox or dead link, and is bounded by
    /// [`RELAY_COMMAND_TIMEOUT`], because callers await this inline in a
    /// consumer's read loop.
    pub(crate) async fn command(&self, command: Command) -> CommandResult {
        self.command_inner(command, None).await
    }

    /// Relay `command` and register `subscription` atomically with the
    /// enqueue (for `SUBSCRIBE_RESOURCE_EVENTS` and `ATTACH_RESOURCE`).
    pub(crate) async fn command_subscribing(
        &self,
        command: Command,
        subscription: ProxySubscription,
    ) -> CommandResult {
        self.command_inner(command, Some(subscription)).await
    }

    /// Relay `command` from consumer `actor`. Keyed commands go through the
    /// capability check and incarnation fence (L1 §9.1); others relay as
    /// [`Self::command`].
    pub(crate) async fn command_from(&self, command: Command, actor: ClientId) -> CommandResult {
        if super::operation_fence::fenced_key(&command).is_none() {
            return self.command(command).await;
        }
        let (reply, rx) = oneshot::channel();
        self.send_and_await(
            RelayRequest::Keyed {
                command,
                actor,
                reply,
            },
            rx,
        )
        .await
    }

    async fn command_inner(
        &self,
        command: Command,
        subscribe: Option<ProxySubscription>,
    ) -> CommandResult {
        // Stamp the issue-order token before it can race a later detach.
        let subscribe = subscribe.map(|mut sub| {
            sub.seq = self.next_seq();
            sub
        });
        let (reply, rx) = oneshot::channel();
        self.send_and_await(
            RelayRequest::Command {
                command,
                reply,
                subscribe,
            },
            rx,
        )
        .await
    }

    /// Enqueue a correlated relay request and await its reply, failing fast
    /// on a saturated or dead link and bounded by [`RELAY_COMMAND_TIMEOUT`].
    async fn send_and_await(
        &self,
        request: RelayRequest,
        rx: oneshot::Receiver<CommandResult>,
    ) -> CommandResult {
        match self.tx.try_send(request) {
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

    /// Relay a `SPAWN_RESOURCE` with the same fail-fast, bounded contract as
    /// [`Self::command`]: a full mailbox is `SpawnFailed` (retryable), a dead
    /// or silent link `SatelliteUnreachable`.
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

    /// Relay a `LIST_DIRECTORY`, fail-fast and bounded at
    /// [`RELAY_LIST_DEADLINE`]; every failure names the host.
    pub(crate) async fn list_directory(&self, path: String) -> DirectoryListingResult {
        let attempted = path.clone();
        let (reply, rx) = oneshot::channel();
        if let Err(err) = self
            .tx
            .try_send(RelayRequest::ListDirectory { path, reply })
        {
            let why = match err {
                mpsc::error::TrySendError::Full(_) => "link is saturated; retry",
                mpsc::error::TrySendError::Closed(_) => "link is down",
            };
            return Err(listing_refusal(
                &attempted,
                format!("satellite {} {why}", self.host),
            ));
        }
        match tokio::time::timeout(RELAY_LIST_DEADLINE, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(listing_refusal(
                &attempted,
                format!("satellite {} link dropped before the listing", self.host),
            )),
            Err(_) => Err(listing_refusal(
                &attempted,
                format!(
                    "satellite {} did not answer the listing within {}s",
                    self.host,
                    RELAY_LIST_DEADLINE.as_secs()
                ),
            )),
        }
    }

    /// Query paths on this satellite, with a bounded typed refusal on failure.
    pub(crate) async fn path_query(
        &self,
        root: String,
        query: String,
        recursive: bool,
    ) -> PathQueryResult {
        let attempted = root.clone();
        let (reply, rx) = oneshot::channel();
        if let Err(err) = self.tx.try_send(RelayRequest::PathQuery {
            root,
            query,
            recursive,
            reply,
        }) {
            let why = match err {
                mpsc::error::TrySendError::Full(_) => "link is saturated; retry",
                mpsc::error::TrySendError::Closed(_) => "link is down",
            };
            return Err(path_refusal(
                &attempted,
                format!("satellite {} {why}", self.host),
            ));
        }
        match tokio::time::timeout(RELAY_LIST_DEADLINE, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(path_refusal(
                &attempted,
                format!(
                    "satellite {} link dropped before the path results",
                    self.host
                ),
            )),
            Err(_) => Err(path_refusal(
                &attempted,
                format!(
                    "satellite {} did not answer the path query within {}s",
                    self.host,
                    RELAY_LIST_DEADLINE.as_secs()
                ),
            )),
        }
    }

    /// Relay `command` without awaiting (`KILL_RESOURCES` tolerates a skip).
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

    /// Relay an uncorrelated frame; drops are logged by semantic class and
    /// never retried.
    pub(crate) fn forward(&self, frame: FrameKind) {
        let category = forward_drop_category(&frame);
        if let Err(err) = self.tx.try_send(RelayRequest::Forward { frame }) {
            warn!(
                satellite = %self.host,
                reason = %trysend_reason(&err),
                category,
                disposition = "dropped_before_satellite",
                retry = false,
                "satellite relay frame was not forwarded"
            );
        }
    }

    /// Register a proxy subscription and forward `forward` atomically. On a
    /// full or dead link nothing is registered, the consumer gets a typed
    /// `ERROR` push, and this returns `false` so the caller rolls back.
    pub(crate) fn subscribe(
        &self,
        mut subscription: ProxySubscription,
        forward: FrameKind,
    ) -> bool {
        subscription.seq = self.next_seq();
        let consumer = subscription.out_tx.clone();
        let terminal = subscription.terminal;
        let Err(err) = self.tx.try_send(RelayRequest::Subscribe {
            subscription,
            forward,
        }) else {
            return true;
        };
        {
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
        false
    }

    /// Ask the link to mirror `terminal`'s agent metadata (ADR-0136). A full
    /// or dead mailbox drops the request; the next inventory retries.
    pub(crate) fn mirror_terminal(&self, terminal: u32) {
        if let Err(err) = self.tx.try_send(RelayRequest::MirrorTerminal { terminal }) {
            trace!(
                satellite = %self.host,
                terminal,
                reason = %trysend_reason(&err),
                "agent metadata mirror was not queued"
            );
        }
    }

    /// Drop every proxy subscription `client` holds on this link, via the
    /// undroppable unsubscribe channel.
    pub(crate) fn unsubscribe_client(&self, client: ClientId) {
        let _ = self.unsub_tx.send(Unsubscribe::Client(client));
    }

    /// Drop `client`'s subscription to one terminal and wait for the proxy
    /// withdrawal, bounded by [`RELAY_COMMAND_TIMEOUT`].
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
        // A dropped receipt means no live session, hence no proxy. A timeout
        // cancels an unapplied withdrawal; if it already applied, report that.
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

const fn forward_drop_category(frame: &FrameKind) -> &'static str {
    match frame {
        FrameKind::InputKey { .. }
        | FrameKind::InputMouse { .. }
        | FrameKind::InputFocus { .. }
        | FrameKind::InputPaste { .. } => "input_unacknowledged",
        FrameKind::FrameAck { .. } => "state_ack",
        _ => "uncorrelated_control",
    }
}

/// Shared registry of per-satellite [`RelayHandle`]s; empty on a non-hub
/// server.
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

/// Fail one queued request while the link is not connected.
pub(crate) fn fail_fast(request: RelayRequest, host: &SatelliteHost, why: &str) {
    match request {
        // Nothing was registered; failing the oneshot is enough.
        RelayRequest::Command { reply, .. } | RelayRequest::Keyed { reply, .. } => {
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
        RelayRequest::ListDirectory { path, reply } => {
            let _ = reply.send(Err(listing_refusal(
                &path,
                format!("satellite {host} is unreachable: {why}"),
            )));
        }
        RelayRequest::PathQuery { root, reply, .. } => {
            let _ = reply.send(Err(path_refusal(
                &root,
                format!("satellite {host} is unreachable: {why}"),
            )));
        }
        RelayRequest::Subscribe { subscription, .. } => {
            // Notify like a disconnect would; nothing to roll back.
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
        RelayRequest::MirrorTerminal { terminal } => {
            // Not marked mirrored; the next inventory retries.
            trace!(
                satellite = %host,
                terminal,
                why,
                "agent metadata mirror dropped while disconnected"
            );
        }
    }
}

/// The typed `ERROR` for an unreachable satellite.
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
        FrameKind::ResizeTerminal { .. } => "RESIZE_TERMINAL",
        FrameKind::SubscribeEvents { .. } => "SUBSCRIBE_EVENTS",
        _ => "other",
    }
}

/// One proxy subscriber on the return leg.
#[derive(Debug)]
struct ProxySubscriber {
    client: ClientId,
    out_tx: mpsc::Sender<Outbound>,
    consumer_cancel: CancellationToken,
    /// Token of the registration held for this `(terminal, client)`.
    seq: u64,
    /// Snapshot-ordering gate (L1 §9.1); see [`SnapshotGate`].
    gate: SnapshotGate,
    /// Remove only after the queued `RESOURCE_CLOSED` is delivered.
    retire_after_flush: bool,
    /// Latest generation identity, so overflow can emit a terminal-scoped
    /// tombstone.
    current_generation: Option<(StreamId, BootstrapId, u64)>,
}

/// Per-subscriber return-leg gate: content deltas flow only after the
/// subscriber's own attach snapshot lands (L1 §9.1), without stalling the
/// link for one slow consumer.
#[derive(Debug)]
enum SnapshotGate {
    /// Attached, but its own snapshot has not been delivered: deltas are
    /// suppressed, so a second attacher never sees the ongoing stream's
    /// output first. The first snapshot moves to `Open` (or `Retained`).
    AwaitingFirst,
    /// Snapshot delivered (or event-only): deltas flow.
    Open,
    /// Bootstrap frames a full mailbox refused, bounded per subscriber and
    /// per connection; exceeding either reaps the subscriber.
    Retained {
        frames: VecDeque<FrameKind>,
        retained_bytes: usize,
        open_after: bool,
    },
    /// Delivery overflowed. The subscriber stays until its resource-scoped
    /// failure is delivered or the deadline passes. Live streams detach
    /// upstream; an already-closed resource does not.
    Retiring {
        failure: FrameKind,
        deadline: std::time::Instant,
        detach_upstream: bool,
    },
}

struct DeliveryFailure {
    frame: FrameKind,
    detach_upstream: bool,
}

#[derive(Clone, Copy)]
enum FanOutKind {
    Bootstrap {
        open_after: bool,
        replace_legacy_begin: bool,
    },
    Content,
}

struct FanOutFrame<'a> {
    host: &'a SatelliteHost,
    terminal: u32,
    frame: &'a FrameKind,
    kind: FanOutKind,
}

/// One in-flight relayed command, plus what to roll back on an error reply.
#[derive(Debug)]
struct PendingCommand {
    reply: oneshot::Sender<CommandResult>,
    /// `(terminal, client, effect)` of the subscription rider.
    subscription: Option<(u32, ClientId, Registration)>,
}

/// What registering a subscription rider did, for precise rollback.
#[derive(Debug, Clone, Copy)]
enum Registration {
    /// A new `(terminal, client)` subscriber; an error removes it.
    New,
    /// A re-subscribe that upgraded an `Open` subscriber to a snapshot
    /// attach and re-gated it. An error restores `Open` if still
    /// `AwaitingFirst` (a snapshot retained meanwhile stays gated).
    Regated,
    /// A re-subscribe that left the gate alone; an error undoes nothing.
    Idempotent,
}

/// One in-flight bootstrap generation on the return leg.
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

/// Reject a chunk whose identity drifted from the in-flight generation or
/// that arrives after READY.
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

/// Charge one frame against `limit`, or reject a satellite over `budget`.
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

/// Charge `payload_len` bytes against `limit`, or reject.
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
const fn stream_frame_scope(frame: &FrameKind) -> Option<&ResourceId> {
    match frame {
        FrameKind::Event { terminal, .. } => terminal.as_ref(),
        FrameKind::ResourceOutput { terminal_id, .. }
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
/// A relayed `LIST_DIRECTORY` awaiting its `DIRECTORY_LISTING`.
#[derive(Debug)]
struct PendingListing {
    /// The requested path, for a refusal that has to name it.
    path: String,
    reply: oneshot::Sender<DirectoryListingResult>,
}

#[derive(Debug)]
struct PendingPathQuery {
    root: String,
    reply: oneshot::Sender<PathQueryResult>,
}

#[derive(Debug)]
struct PendingDetach {
    terminal: u32,
    deadline: tokio::time::Instant,
}

/// Per-connection relay state driven by [`super::link::run_link`].
///
/// Owns link-side request ids, pending replies, and the proxy registry.
/// Session-scoped: [`Self::teardown`] fails pending work so a reconnect
/// starts clean.
#[derive(Debug)]
pub(crate) struct RelaySession {
    host: SatelliteHost,
    bootstrap_limits: BootstrapLimits,
    bootstrap_profile: BootstrapProfile,
    next_request_id: u32,
    pending: HashMap<u32, PendingCommand>,
    enforce_bootstrap_flow: bool,
    /// Relayed spawns awaiting `RESOURCE_SPAWNED`, in the shared id space.
    pending_spawns: HashMap<u32, oneshot::Sender<SpawnResult>>,
    /// Relayed listings awaiting `DIRECTORY_LISTING`, in the same id space.
    pending_listings: HashMap<u32, PendingListing>,
    pending_paths: HashMap<u32, PendingPathQuery>,
    /// The satellite's `HELLO_OK` features; listings need `LIST_DIRECTORY`.
    satellite_features: ServerFeatureSet,
    satellite_features_ext: ServerFeatureExtSet,
    /// Upstream detach barriers: old frames may arrive until the reply.
    pending_detaches: HashMap<u32, PendingDetach>,
    /// Terminals whose metadata allowlist is already mirrored (ADR-0136).
    mirrored: HashSet<u32>,
    /// Link-side `GET_METADATA` ids for that allowlist: terminal and key.
    pending_mirror_gets: HashMap<u32, (u32, String)>,
    subscribers: HashMap<u32, Vec<ProxySubscriber>>,
    /// Terminals with an explicit content attach since the last upstream
    /// detach; a refused attach cannot resurrect the retired SPAWN
    /// generation.
    explicit_content: HashSet<u32>,
    /// Legacy `SUBSCRIBE_EVENTS` terminals, re-subscribed after internal
    /// detaches (which remove them upstream).
    legacy_events: HashSet<u32>,
    bootstrap_flows: HashMap<u32, RelayBootstrapFlow>,
    retained_bytes: usize,
    retained_frames: usize,
    /// Terminals whose last subscriber was removed by delivery failure,
    /// drained by the link into upstream detaches.
    orphaned_by_delivery: HashSet<u32>,
    inflight_generation_bytes: u64,
    inflight_generation_frames: u32,
    encode_buf: BytesMut,
    /// Hub state for re-stamping relayed events; `None` keeps the satellite's.
    journal: Option<crate::state::SharedState>,
    /// The satellite's `HELLO_OK.server_id` on this connection.
    incarnation: super::operation_fence::Incarnation,
    /// Incarnation fence shared with the link's other sessions (L1 §9.1).
    operations: super::operation_fence::OperationFence,
}

impl RelaySession {
    /// Fresh session state for one established connection to `host`.
    #[cfg(test)]
    pub(crate) fn new(host: SatelliteHost, bootstrap_limits: BootstrapLimits) -> Self {
        let mut session = Self::new_negotiated(
            host,
            bootstrap_limits,
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        session.enforce_bootstrap_flow = false;
        session
    }

    /// Fresh session state pinned to the connection's exact negotiated profile.
    pub(crate) fn new_negotiated(
        host: SatelliteHost,
        bootstrap_limits: BootstrapLimits,
        bootstrap_profile: BootstrapProfile,
        satellite_features: ServerFeatureSet,
    ) -> Self {
        Self {
            host,
            bootstrap_limits,
            bootstrap_profile,
            next_request_id: 1,
            pending: HashMap::new(),
            pending_spawns: HashMap::new(),
            pending_listings: HashMap::new(),
            pending_paths: HashMap::new(),
            satellite_features,
            satellite_features_ext: ServerFeatureExtSet::new(),
            pending_detaches: HashMap::new(),
            mirrored: HashSet::new(),
            pending_mirror_gets: HashMap::new(),
            enforce_bootstrap_flow: true,
            subscribers: HashMap::new(),
            explicit_content: HashSet::new(),
            legacy_events: HashSet::new(),
            bootstrap_flows: HashMap::new(),
            retained_bytes: 0,
            retained_frames: 0,
            orphaned_by_delivery: HashSet::new(),
            inflight_generation_bytes: 0,
            inflight_generation_frames: 0,
            encode_buf: BytesMut::with_capacity(1024),
            journal: None,
            incarnation: None,
            operations: super::operation_fence::OperationFence::default(),
        }
    }

    /// Re-stamp relayed events from `journal`, the hub's own state.
    pub(crate) fn set_journal(&mut self, journal: Option<crate::state::SharedState>) {
        self.journal = journal;
    }

    /// Record the incarnation this connection's satellite announced.
    pub(crate) const fn set_incarnation(
        &mut self,
        incarnation: super::operation_fence::Incarnation,
    ) {
        self.incarnation = incarnation;
    }

    /// Share the link's incarnation fence, which outlives this connection.
    pub(crate) fn set_operation_fence(
        &mut self,
        operations: super::operation_fence::OperationFence,
    ) {
        self.operations = operations;
    }

    pub(crate) const fn set_satellite_features_ext(&mut self, features: ServerFeatureExtSet) {
        self.satellite_features_ext = features;
    }

    /// Before a first explicit attach, fence any automatic SPAWN generation
    /// upstream (no proxy owns it); shared subscriptions keep streaming.
    pub(crate) fn prepare_request(&mut self, request: &RelayRequest) -> Vec<Vec<u8>> {
        if let RelayRequest::MirrorTerminal { terminal } = request {
            return self.mirror_terminal_frames(*terminal);
        }
        let RelayRequest::Command {
            command: Command::AttachResource { .. },
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
            let cursor = self.link_event_cursor();
            frames.push(self.encode(&FrameKind::SubscribeEvents {
                terminal: Some(ResourceId::local(terminal)),
                after_seq: cursor,
            }));
        }
        frames
    }

    /// Service one request. `None`: rejected locally, consumer already told.
    pub(crate) fn handle_request_checked(&mut self, request: RelayRequest) -> Option<Vec<u8>> {
        match request {
            RelayRequest::Command {
                command,
                reply,
                subscribe,
            } => {
                if let Some(refusal) = self.command_rejection(&command) {
                    let _ = reply.send(refusal);
                    return None;
                }
                if let Some(sub) = subscribe.as_ref()
                    && let Some((code, message)) = self.subscription_rejection(sub)
                {
                    let _ = reply.send(CommandResult::Error { code, message });
                    return None;
                }
                // Register the rider with the enqueue, remembering how to
                // roll it back.
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
                    command: link_attach_command(command, self.satellite_features),
                }))
            }
            RelayRequest::Keyed {
                command,
                actor,
                reply,
            } => {
                if let Some(refusal) = self
                    .command_rejection(&command)
                    .or_else(|| self.fence_rejection(&command, actor))
                {
                    let _ = reply.send(refusal);
                    return None;
                }
                let request_id = self.allocate_request_id();
                self.pending.insert(
                    request_id,
                    PendingCommand {
                        reply,
                        subscription: None,
                    },
                );
                Some(self.encode(&FrameKind::Command {
                    request_id,
                    command,
                }))
            }
            RelayRequest::Forward { frame } => Some(self.encode(&frame)),
            RelayRequest::Spawn { spawn, reply } => {
                if let Some(refusal) = self.spawn_capability_rejection(&spawn) {
                    let _ = reply.send(SpawnResult::Err(refusal));
                    return None;
                }
                let request_id = self.allocate_request_id();
                self.pending_spawns.insert(request_id, reply);
                Some(self.encode(&FrameKind::SpawnResource {
                    request_id,
                    group: spawn.group,
                    command: spawn.command,
                    cwd: spawn.cwd,
                    env: spawn.env,
                    term: spawn.term,
                    // Hub-and-spoke: the satellite spawns locally.
                    satellite: None,
                    owner_terminal: spawn.owner_terminal.map(ResourceId::local),
                    agent_session: None,
                    initial_size: spawn.initial_size,
                    // Kind and retagged parent cross intact (ADR-0104 §6).
                    resource: spawn.resource,
                }))
            }
            RelayRequest::Subscribe {
                subscription,
                forward,
            } => self.subscribe_forward(subscription, &forward),
            RelayRequest::ListDirectory { path, reply } => self.enqueue_listing(path, reply),
            RelayRequest::PathQuery {
                root,
                query,
                recursive,
                reply,
            } => self.enqueue_path_query(root, query, recursive, reply),
            // Frames were produced by [`Self::prepare_request`].
            RelayRequest::MirrorTerminal { .. } => None,
        }
    }

    /// `SUBSCRIBE_METADATA` plus a `GET_METADATA` per allowlisted key, once
    /// per terminal per connection (`METADATA_VALUE` carries no key, hence
    /// [`Self::pending_mirror_gets`]).
    fn mirror_terminal_frames(&mut self, terminal: u32) -> Vec<Vec<u8>> {
        if !self.mirrored.insert(terminal) {
            return Vec::new();
        }
        let scope = Scope::Resource(ResourceId::local(terminal));
        let mut frames = Vec::new();
        for key in crate::hub::metadata_mirror::MIRROR_KEYS {
            frames.push(self.encode(&FrameKind::SubscribeMetadata {
                scope: scope.clone(),
                key: (*key).to_owned(),
            }));
            let request_id = self.allocate_request_id();
            self.pending_mirror_gets
                .insert(request_id, (terminal, (*key).to_owned()));
            frames.push(self.encode(&FrameKind::GetMetadata {
                request_id,
                scope: scope.clone(),
                key: (*key).to_owned(),
            }));
        }
        frames
    }

    /// The satellite's `HELLO_OK` features (ADR-0127).
    pub(crate) const fn satellite_features(&self) -> ServerFeatureSet {
        self.satellite_features
    }

    /// Refuse a spawn with fields the satellite would silently skip.
    fn spawn_capability_rejection(&self, spawn: &SatelliteSpawn) -> Option<SpawnError> {
        let resource = spawn.resource.as_ref()?;
        if resource.idempotency_key.is_some()
            && !self
                .satellite_features
                .contains(ServerFeature::SpawnIdempotency)
        {
            return Some(SpawnError::SpawnFailed(format!(
                "satellite {} lacks SPAWN_IDEMPOTENCY; nothing was spawned",
                self.host
            )));
        }
        if resource.retain_secs.is_some()
            && !self
                .satellite_features
                .contains(ServerFeature::RetainOnExit)
        {
            return Some(SpawnError::SpawnFailed(format!(
                "satellite {} lacks RETAIN_ON_EXIT; nothing was spawned",
                self.host
            )));
        }
        None
    }

    /// Refuse, without touching the link, a command this satellite cannot
    /// evaluate (conditional kill, or a keyed op without dedupe).
    fn command_rejection(&self, command: &Command) -> Option<CommandResult> {
        if let Some(message) = self.conditional_kill_rejection(command) {
            return Some(CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                message,
            });
        }
        self.keyed_capability_rejection(command)
    }

    /// A keyed operation reaches only a satellite that dedupes it
    /// (`ACKNOWLEDGED_INPUT` / `KEYED_SIGNAL`), else
    /// `UNSUPPORTED_SATELLITE_ROUTE` (L1 §9.1).
    fn keyed_capability_rejection(&self, command: &Command) -> Option<CommandResult> {
        let (needed, name) = match command {
            Command::ApplyInput { .. } => (ServerFeature::AcknowledgedInput, "ACKNOWLEDGED_INPUT"),
            _ if command.idempotency_key().is_some() => {
                (ServerFeature::KeyedSignal, "KEYED_SIGNAL")
            }
            _ => return None,
        };
        if self.satellite_features.contains(needed) {
            return None;
        }
        Some(CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!(
                "satellite {} lacks {name}, so it cannot deduplicate this operation; \
                 nothing was forwarded",
                self.host
            ),
        })
    }

    /// The incarnation fence (L1 §9.1): an operation id already sent to an
    /// earlier incarnation is refused `INCARNATION_CHANGED`; a new id is
    /// recorded with `actor`.
    fn fence_rejection(&self, command: &Command, actor: ClientId) -> Option<CommandResult> {
        use super::operation_fence::{FenceVerdict, fenced_key, fenced_terminals};
        let key = fenced_key(command)?;
        match self.operations.admit(
            key,
            self.incarnation,
            actor,
            fenced_terminals(command),
            std::time::Instant::now(),
        ) {
            FenceVerdict::Forward => None,
            FenceVerdict::IncarnationChanged => Some(CommandResult::Error {
                code: ErrorCode::IncarnationChanged,
                message: format!(
                    "satellite {} restarted since this operation id was first forwarded; \
                     nothing was forwarded",
                    self.host
                ),
            }),
            FenceVerdict::Full => Some(CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: format!(
                    "the hub's operation record for satellite {} is full; retry later",
                    self.host
                ),
            }),
        }
    }

    /// Refuse `KILL_RESOURCE_IF` to a satellite without `CONDITIONAL_KILL`
    /// (ADR-0109) instead of letting the consumer time out.
    fn conditional_kill_rejection(&self, command: &Command) -> Option<String> {
        let Command::KillResourceIf { .. } = command else {
            return None;
        };
        if self
            .satellite_features
            .contains(ServerFeature::ConditionalKill)
        {
            return None;
        }
        Some(format!(
            "satellite {} cannot evaluate a conditional kill; nothing was killed",
            self.host
        ))
    }

    /// Put one `LIST_DIRECTORY` on the wire, or refuse it when the satellite
    /// never advertised the query.
    fn enqueue_listing(
        &mut self,
        path: String,
        reply: oneshot::Sender<DirectoryListingResult>,
    ) -> Option<Vec<u8>> {
        if !self
            .satellite_features
            .contains(ServerFeature::ListDirectory)
        {
            let _ = reply.send(Err(listing_refusal(
                &path,
                format!("satellite {} does not answer LIST_DIRECTORY", self.host),
            )));
            return None;
        }
        let request_id = self.allocate_request_id();
        let frame = FrameKind::ListDirectory {
            request_id,
            path: path.clone(),
            host: None,
        };
        self.pending_listings
            .insert(request_id, PendingListing { path, reply });
        Some(self.encode(&frame))
    }

    fn enqueue_path_query(
        &mut self,
        root: String,
        query: String,
        recursive: bool,
        reply: oneshot::Sender<PathQueryResult>,
    ) -> Option<Vec<u8>> {
        if !self
            .satellite_features_ext
            .contains(ServerFeatureExt::PathQuery)
        {
            let _ = reply.send(Err(path_refusal(
                &root,
                format!("satellite {} does not answer PATH_QUERY", self.host),
            )));
            return None;
        }
        let request_id = self.allocate_request_id();
        let frame = FrameKind::PathQuery {
            request_id,
            root: root.clone(),
            query,
            recursive,
            host: None,
        };
        self.pending_paths
            .insert(request_id, PendingPathQuery { root, reply });
        Some(self.encode(&frame))
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
            terminal: Some(ResourceId::Local { id }),
            ..
        } = forward
        {
            self.legacy_events.insert(*id);
        }
        self.register_subscriber(subscription);
        let forward = self.with_link_cursor(forward);
        Some(self.encode(&forward))
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

    /// Register one proxy subscriber idempotently, returning the
    /// [`Registration`] effect for rollback.
    fn register_subscriber(&mut self, subscription: ProxySubscription) -> Registration {
        let ProxySubscription {
            terminal,
            client,
            out_tx,
            consumer_cancel,
            seq,
            awaits_snapshot,
            bootstrap_profile: _,
            bootstrap_limits: _,
        } = subscription;
        self.orphaned_by_delivery.remove(&terminal);
        let subs = self.subscribers.entry(terminal).or_default();
        if let Some(existing) = subs.iter_mut().find(|s| s.client == client) {
            existing.out_tx = out_tx;
            existing.consumer_cancel = consumer_cancel;
            // Keep the freshest token.
            existing.seq = existing.seq.max(seq);
            // An upgrade from an `Open` stream to a snapshot attach re-gates
            // until this attach's snapshot lands (L1 §9.1). Other gates
            // already suppress deltas; event-only re-subscribes leave it.
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
                consumer_cancel,
                seq,
                // Attaches gate until their snapshot; event-only opens now.
                gate: if awaits_snapshot {
                    SnapshotGate::AwaitingFirst
                } else {
                    SnapshotGate::Open
                },
                retire_after_flush: false,
                current_generation: None,
            });
            Registration::New
        }
    }

    /// Withdraw proxy subscriptions. Returns the upstream
    /// `DETACH_RESOURCE` frames for terminals that lost their last proxy (or
    /// had none after an automatic spawn). The upstream reply retires the old
    /// bootstrap flow; the downstream receipt certifies only local removal.
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
        // A relayed SPAWN streams before any proxy attaches; detach stops it.
        let Some(subs) = self.subscribers.get_mut(&terminal) else {
            return Ok(Some(terminal));
        };
        // Never let a stale detach remove a newer registration.
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
            command: Command::DetachResource {
                terminal_id: ResourceId::local(terminal),
            },
        })
    }

    /// Dispatch one frame from the satellite: resolve replies, re-tag and fan
    /// out streams.
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
            FrameKind::ResourceSpawned { request_id, result } => {
                self.resolve_pending_spawn(request_id, result);
            }
            FrameKind::DirectoryListing { request_id, result } => {
                self.resolve_pending_listing(request_id, result);
            }
            FrameKind::PathResults { request_id, result } => {
                self.resolve_pending_path(request_id, result);
            }
            FrameKind::Error {
                request_id: Some(request_id),
                code,
                message,
            } => self.resolve_correlated_error(request_id, code, message),
            FrameKind::Event { .. }
            | FrameKind::ResourceOutput { .. }
            | FrameKind::BootstrapBegin { .. }
            | FrameKind::BootstrapChunk { .. }
            | FrameKind::BootstrapReady { .. }
            | FrameKind::HistoryPage { .. }
            | FrameKind::BootstrapTombstone { .. }
            | FrameKind::HistoryTombstone { .. }
            | FrameKind::HistoryRejected { .. } => self.relay_stream_frame(frame)?,
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => self.relay_terminal_closed(&terminal_id, exit_status, reason, signal),
            FrameKind::Bell { terminal_id } => self.relay_bell(&terminal_id),
            FrameKind::MetadataChanged {
                scope, key, value, ..
            } => self.apply_mirrored_metadata(&scope, &key, value),
            FrameKind::MetadataValue { request_id, value } => {
                self.apply_mirrored_value(request_id, value);
            }
            // Other metadata frames are not part of the mirror; absorb them.
            FrameKind::GetMetadata { .. }
            | FrameKind::SetMetadata { .. }
            | FrameKind::DeleteMetadata { .. }
            | FrameKind::ListMetadata { .. }
            | FrameKind::SubscribeMetadata { .. }
            | FrameKind::MetadataKeys { .. } => {}
            other => {
                return Err(format!(
                    "satellite {} sent a direction-invalid frame after HELLO_OK: {other:?}",
                    self.host
                ));
            }
        }
        Ok(())
    }

    /// The old upstream generation stays fenced until a successful detach
    /// receipt; a refusal cannot authorize a fresh stream.
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

    /// Resolve a correlated `ERROR` against whichever request holds the id
    /// (a satellite may answer a spawn with a generic error).
    fn resolve_correlated_error(&mut self, request_id: u32, code: ErrorCode, message: String) {
        if self.pending_mirror_gets.remove(&request_id).is_some() {
            debug!(
                satellite = %self.host,
                request_id,
                ?code,
                %message,
                "satellite refused a mirrored metadata read; leaving the key unset"
            );
            return;
        }
        if self.pending_spawns.contains_key(&request_id) {
            self.resolve_pending_spawn(
                request_id,
                SpawnResult::Err(SpawnError::SpawnFailed(format!(
                    "satellite refused the spawn: {code:?}: {message}"
                ))),
            );
            return;
        }
        if let Some(pending) = self.pending_listings.remove(&request_id) {
            let refusal = listing_refusal(
                &pending.path,
                format!(
                    "satellite {} refused the listing: {code:?}: {message}",
                    self.host
                ),
            );
            let _ = pending.reply.send(Err(refusal));
            return;
        }
        if let Some(pending) = self.pending_paths.remove(&request_id) {
            let _ = pending.reply.send(Err(path_refusal(
                &pending.root,
                format!(
                    "satellite {} refused the path query: {code:?}: {message}",
                    self.host
                ),
            )));
            return;
        }
        self.resolve_pending(request_id, CommandResult::Error { code, message });
    }

    /// Resolve a listing id to its waiting consumer; paths pass through.
    fn resolve_pending_listing(&mut self, request_id: u32, result: DirectoryListingResult) {
        let Some(pending) = self.pending_listings.remove(&request_id) else {
            debug!(
                satellite = %self.host,
                request_id,
                "satellite directory listing with no pending request; dropping"
            );
            return;
        };
        // A dropped receiver (consumer timed out / disconnected) is fine.
        let _ = pending.reply.send(result);
    }

    fn resolve_pending_path(&mut self, request_id: u32, result: PathQueryResult) {
        if let Some(pending) = self.pending_paths.remove(&request_id) {
            let _ = pending.reply.send(result);
        } else {
            debug!(satellite = %self.host, request_id, "satellite path results with no pending request; dropping");
        }
    }

    /// Forward one terminal-scoped return-leg frame: check its flow gate,
    /// re-tag, fan out.
    fn relay_stream_frame(&mut self, frame: FrameKind) -> Result<(), String> {
        if let Some(dropped) = link_journal_gap(&frame) {
            self.relay_link_gap(dropped);
            return Ok(());
        }
        let Some(id) = self.retag_inbound(stream_frame_scope(&frame)) else {
            return Ok(());
        };
        if !matches!(frame, FrameKind::Event { .. })
            && self
                .pending_detaches
                .values()
                .any(|pending| pending.terminal == id)
        {
            // Pre-detach frames; a fresh proxy must not take them as its prefix.
            return Ok(());
        }
        if self.enforce_bootstrap_flow {
            self.enforce_stream_frame_flow(id, &frame)?;
        }
        let retagged = self.retag_stream_frame(frame, id);
        self.deliver_stream_frame(id, retagged);
        Ok(())
    }

    /// Deliver one re-tagged frame. An `EVENT` is re-stamped by the hub's
    /// registry (ADR-0123, L1 §7.3) and reaches that Terminal's subscribers;
    /// without a journal it fans out with its stamp dropped.
    fn deliver_stream_frame(&mut self, id: u32, frame: FrameKind) {
        let FrameKind::Event {
            terminal,
            mut event,
            stamp,
        } = frame
        else {
            self.fan_out(id, &frame);
            return;
        };
        satellite_gap_as_source_gap(&mut event);
        self.mirror_satellite_lease_state(id, &event, stamp.as_deref().map(|s| s.seq));
        // L1 §9.1: a keyed operation's event names the consumer that sent
        // it, and only on a terminal that operation targeted.
        let (stamp, actor) = self.vet_event_stamp(id, stamp);
        let Some(event) = self.retag_event_ids(id, event, actor) else {
            return;
        };
        if let (Some(journal), Some(scope)) = (self.journal.clone(), terminal.clone()) {
            let _ = journal
                .with_mut(|s| s.record_relayed_event_as(scope, event, stamp.as_ref(), actor));
            return;
        }
        let unstamped = FrameKind::Event {
            terminal,
            event,
            stamp: None,
        };
        self.fan_out(id, &unstamped);
    }

    /// The satellite's stamp with every id the hub cannot vouch for removed,
    /// and the hub consumer the event belongs to. An `operation_id` a hub
    /// consumer forwarded for another terminal is dropped (the satellite may
    /// not claim it here); one this link never forwarded is kept but names no
    /// hub consumer; the satellite's own `actor` names its connections, never
    /// the hub's, and never survives.
    fn vet_event_stamp(
        &self,
        terminal: u32,
        stamp: Option<Box<phux_protocol::wire::frame::EventStamp>>,
    ) -> (
        Option<phux_protocol::wire::frame::EventStamp>,
        Option<ClientId>,
    ) {
        use super::operation_fence::EventOperation;
        let Some(mut stamp) = stamp.map(|stamp| *stamp) else {
            return (None, None);
        };
        stamp.actor = None;
        let verdict = stamp
            .operation_id
            .map(|key| self.operations.event_operation(&key, terminal));
        let actor = match verdict {
            Some(EventOperation::Forwarded(actor)) => Some(actor),
            Some(EventOperation::Misattributed) => {
                warn!(
                    satellite = %self.host,
                    terminal,
                    "satellite stamped a forwarded operation id on another terminal's event; dropping the id"
                );
                stamp.operation_id = None;
                None
            }
            Some(EventOperation::Unknown) | None => None,
        };
        (Some(stamp), actor)
    }

    /// Re-tag the ids inside a satellite event into the hub's id space
    /// (L1 §9.1), or `None` to drop an event naming an id the hub cannot
    /// represent: a resource parent is re-tagged `Satellite { host, id }`
    /// (a chained one drops the event); a client id becomes the hub
    /// consumer the hub knows for it (the lease holder, the operation's
    /// sender) or a [`foreign_client`] id; an approval event naming one of
    /// the hub's own pending approvals is dropped, since approval ids share
    /// one namespace and the satellite's must never decide or shadow the
    /// hub's.
    fn retag_event_ids(
        &self,
        terminal: u32,
        event: AgentEvent,
        operation_actor: Option<ClientId>,
    ) -> Option<AgentEvent> {
        match event {
            AgentEvent::ResourceSpawned { kind, parent } => {
                let parent = match parent {
                    None => None,
                    Some(ResourceId::Local { id }) => {
                        Some(ResourceId::satellite(self.host.clone(), id))
                    }
                    Some(ResourceId::Satellite { .. }) => {
                        warn!(
                            satellite = %self.host,
                            terminal,
                            "satellite reported a Satellite-tagged parent; hub-and-spoke does not chain — dropping"
                        );
                        return None;
                    }
                };
                Some(AgentEvent::ResourceSpawned { kind, parent })
            }
            AgentEvent::TerminalControl {
                lifecycle,
                exit_status,
                input_holder,
                action,
                actor,
            } => {
                let holder = self.journal.as_ref().and_then(|journal| {
                    journal.with(|s| s.satellite_lease_holder(&self.host, terminal))
                });
                Some(AgentEvent::TerminalControl {
                    lifecycle,
                    exit_status,
                    input_holder: input_holder.map(|id| {
                        holder.map_or_else(|| foreign_client(id), crate::runtime::wire_client)
                    }),
                    action,
                    actor: actor.map(|id| {
                        operation_actor
                            .map_or_else(|| foreign_client(id), crate::runtime::wire_client)
                    }),
                })
            }
            AgentEvent::ApprovalRequested { id } | AgentEvent::ApprovalDecided { id, .. }
                if self.names_hub_approval(id) =>
            {
                warn!(
                    satellite = %self.host,
                    terminal,
                    "satellite event named one of the hub's pending approvals; dropping"
                );
                None
            }
            other => Some(other),
        }
    }

    /// Whether `id` is one of the hub's own pending approvals.
    fn names_hub_approval(&self, id: phux_protocol::ids::ApprovalId) -> bool {
        self.journal
            .as_ref()
            .is_some_and(|journal| journal.with(|s| s.pending_approval(id).is_some()))
    }

    /// Mirror a satellite lease transition into the hub's ledger (ADR-0033):
    /// the satellite owns the timer, the hub reflects it.
    ///
    /// Acquired/Seized never change the holder by themselves (the hub's own
    /// `ACQUIRE_INPUT` reply installs it) but clear a pending mark and record
    /// `seq`. Released/Expired may evict, except while an acquire reply is
    /// pending or when `seq` is not newer: reply and event share the link but
    /// are not ordered against each other.
    fn mirror_satellite_lease_state(&self, id: u32, event: &AgentEvent, seq: Option<u64>) {
        let Some(journal) = &self.journal else {
            return;
        };
        let is_end = match event {
            AgentEvent::TerminalControl {
                action: ControlAction::Acquired | ControlAction::Seized,
                ..
            } => false,
            AgentEvent::TerminalControl {
                action: ControlAction::Expired | ControlAction::Released,
                ..
            } => true,
            _ => return,
        };
        journal.with_mut(|s| s.mirror_satellite_lease_event(&self.host, id, is_end, seq));
    }

    /// Report a satellite's link-level `journal_gap` as a `source_gap` to
    /// every relayed Terminal's consumers. `dropped` is an upper bound.
    fn relay_link_gap(&mut self, dropped: u64) {
        let terminals: Vec<u32> = self.subscribers.keys().copied().collect();
        for id in terminals {
            let gap = FrameKind::Event {
                terminal: Some(ResourceId::satellite(self.host.clone(), id)),
                event: AgentEvent::SourceGap { dropped },
                stamp: None,
            };
            self.deliver_stream_frame(id, gap);
        }
    }

    /// The cursor for the link's own `SUBSCRIBE_EVENTS`: journal semantics
    /// with no replay when the satellite speaks them (L1 §7.1), else live-only.
    fn link_event_cursor(&self) -> Option<u64> {
        self.satellite_features
            .contains(ServerFeature::EventJournal)
            .then_some(u64::MAX)
    }

    /// `forward`, with the link's event cursor when it subscribes events.
    fn with_link_cursor(&self, forward: &FrameKind) -> FrameKind {
        let mut forward = forward.clone();
        if let FrameKind::SubscribeEvents { after_seq, .. } = &mut forward {
            *after_seq = self.link_event_cursor();
        }
        forward
    }

    /// The bootstrap-flow gate each stream frame must clear.
    fn enforce_stream_frame_flow(&mut self, id: u32, frame: &FrameKind) -> Result<(), String> {
        match frame {
            FrameKind::ResourceOutput {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "RESOURCE_OUTPUT"),
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

    /// Re-tag a stream frame's scope `Local { id }` -> `Satellite { host,
    /// id }`, leaving every other field verbatim (ADR-0007).
    fn retag_stream_frame(&self, mut frame: FrameKind, id: u32) -> FrameKind {
        let scope = ResourceId::satellite(self.host.clone(), id);
        match &mut frame {
            FrameKind::Event { terminal, .. } => *terminal = Some(scope),
            FrameKind::ResourceOutput { terminal_id, .. }
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

    /// Deliver `RESOURCE_CLOSED` with the satellite's reason unchanged
    /// (ADR-0104 §4), then reap what the terminal owned on this link.
    fn relay_terminal_closed(
        &mut self,
        terminal_id: &ResourceId,
        exit_status: Option<i32>,
        reason: phux_protocol::wire::frame::CloseReason,
        signal: Option<i32>,
    ) {
        let Some(id) = self.retag_inbound(Some(terminal_id)) else {
            return;
        };
        if let Some(subscribers) = self.subscribers.get_mut(&id) {
            for subscriber in subscribers {
                subscriber.retire_after_flush = true;
            }
        }
        // Past an AwaitingFirst gate but after retained output; subscribers
        // are removed once the close reaches them.
        self.fan_out_ungated(
            id,
            &FrameKind::ResourceClosed {
                terminal_id: ResourceId::satellite(self.host.clone(), id),
                exit_status,
                reason,
                signal,
            },
        );
        // The satellite already declared it gone: no upstream detach.
        self.orphaned_by_delivery.remove(&id);
        self.recalculate_retained_totals();
        self.retire_bootstrap_flow(id);
        self.explicit_content.remove(&id);
        self.legacy_events.remove(&id);
        self.forget_mirror(id);
    }

    /// Deliver one `BELL` side-channel notification.
    fn relay_bell(&mut self, terminal_id: &ResourceId) {
        let Some(id) = self.retag_inbound(Some(terminal_id)) else {
            return;
        };
        // A bell is not in the snapshot, so it bypasses the gate rather than
        // being dropped.
        self.fan_out_ungated(
            id,
            &FrameKind::Bell {
                terminal_id: ResourceId::satellite(self.host.clone(), id),
            },
        );
    }

    /// Fail in-flight commands, notify subscribers, clear registries. Once
    /// per session.
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
        for (_, pending) in self.pending_listings.drain() {
            let _ = pending.reply.send(Err(listing_refusal(
                &pending.path,
                format!("satellite {} is unreachable: {why}", self.host),
            )));
        }
        for (_, pending) in self.pending_paths.drain() {
            let _ = pending.reply.send(Err(path_refusal(
                &pending.root,
                format!("satellite {} is unreachable: {why}", self.host),
            )));
        }
        // One typed ERROR per consumer, naming the host.
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
        self.interrupt_consumer_scopes();
        self.subscribers.clear();
        self.bootstrap_flows.clear();
        self.explicit_content.clear();
        self.legacy_events.clear();
        self.pending_detaches.clear();
        self.mirrored.clear();
        self.pending_mirror_gets.clear();
        self.retained_bytes = 0;
        self.retained_frames = 0;
        self.inflight_generation_bytes = 0;
        self.inflight_generation_frames = 0;
    }

    /// Drop every consumer's satellite scope from the event registry, owing
    /// each a `journal_gap` (ADR-0123).
    fn interrupt_consumer_scopes(&self) {
        let Some(journal) = &self.journal else {
            return;
        };
        for (id, subs) in &self.subscribers {
            let scope = ResourceId::satellite(self.host.clone(), *id);
            for sub in subs {
                journal.with_mut(|s| s.interrupt_satellite_scope(sub.client, &scope));
            }
        }
    }

    /// Drop pending entries whose consumer stopped waiting; returns how many.
    /// Run on the keepalive tick so a satellite that never answers cannot
    /// grow the map without bound.
    pub(crate) fn prune_abandoned(&mut self) -> usize {
        let before = self.pending_request_count();
        self.pending.retain(|_, pending| !pending.reply.is_closed());
        self.pending_spawns.retain(|_, reply| !reply.is_closed());
        self.pending_listings
            .retain(|_, pending| !pending.reply.is_closed());
        self.pending_paths
            .retain(|_, pending| !pending.reply.is_closed());
        let remaining = self.pending_request_count();
        let pruned = before - remaining;
        if pruned > 0 {
            debug!(
                satellite = %self.host,
                pruned,
                remaining,
                "pruned relayed commands whose consumer stopped waiting"
            );
        }
        pruned
    }

    /// Correlated requests still waiting on the satellite, of every kind.
    fn pending_request_count(&self) -> usize {
        self.pending.len()
            + self.pending_spawns.len()
            + self.pending_listings.len()
            + self.pending_paths.len()
    }

    /// A peer that never acknowledges a detach leaves generation ownership
    /// uncertain, so an expired barrier resets the link.
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

    /// Resolve a link-side id to its consumer. An error reply rolls back the
    /// command's subscription rider.
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

    /// Undo a failed subscribing command's registration effect.
    fn roll_back_subscription(&mut self, terminal: u32, client: ClientId, effect: Registration) {
        match effect {
            // The command created it: remove it.
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
            // Restore `Open` after a failed upgrade, but only if this
            // command's gate is still there: a snapshot retained meanwhile
            // must stay ahead of later deltas.
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
            Registration::Idempotent => {}
        }
        self.recalculate_retained_totals();
    }

    /// Resolve a spawn reply, re-tagging the new id to this host; a
    /// `Satellite`-tagged id in the reply becomes `SpawnFailed`.
    fn resolve_pending_spawn(&mut self, request_id: u32, result: SpawnResult) {
        let Some(reply) = self.pending_spawns.remove(&request_id) else {
            debug!(
                satellite = %self.host,
                request_id,
                "satellite spawn reply with no pending spawn; dropping"
            );
            return;
        };
        let retagged = retag_spawn_result(&self.host, result);
        // A dropped receiver (consumer timed out / disconnected) is fine.
        let _ = reply.send(retagged);
    }

    /// The satellite-local id of an inbound frame's scope; `None` when
    /// unscoped or already satellite-tagged (never chained).
    fn retag_inbound(&self, terminal: Option<&ResourceId>) -> Option<u32> {
        match terminal {
            Some(ResourceId::Local { id }) => Some(*id),
            Some(ResourceId::Satellite { .. }) => {
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

    /// Push `frame` to every proxy subscriber of terminal `id` via
    /// `try_send`.
    ///
    /// A `TERMINAL_SNAPSHOT` anchors ordering (L1 §9.1): a new attacher waits
    /// in [`SnapshotGate::AwaitingFirst`] and a refused snapshot is retained
    /// ([`SnapshotGate::Retained`]) and retried before any delta; a newer
    /// snapshot replaces it. `RESOURCE_CLOSED` and `BELL` bypass the gate
    /// ([`Self::fan_out_ungated`]).
    fn fan_out(&mut self, id: u32, frame: &FrameKind) {
        let host = &self.host;
        let Some(subs) = self.subscribers.get_mut(&id) else {
            trace!(satellite = %host, terminal = id, "inbound stream frame with no proxy subscribers");
            return;
        };
        let detach_if_empty = subs.iter().any(Self::detach_upstream_on_removal);
        let is_begin = matches!(frame, FrameKind::BootstrapBegin { .. });
        let kind = if is_begin
            || matches!(
                frame,
                FrameKind::BootstrapChunk { .. }
                    | FrameKind::BootstrapReady { .. }
                    | FrameKind::BootstrapTombstone { .. }
            ) {
            FanOutKind::Bootstrap {
                open_after: matches!(frame, FrameKind::BootstrapReady { .. }),
                replace_legacy_begin: is_begin && !self.enforce_bootstrap_flow,
            }
        } else {
            FanOutKind::Content
        };
        let outbound = FanOutFrame {
            host,
            terminal: id,
            frame,
            kind,
        };
        let mut retained_bytes = self.retained_bytes;
        let mut retained_frames = self.retained_frames;
        subs.retain_mut(|sub| {
            Self::fan_out_to_subscriber(sub, &outbound, &mut retained_bytes, &mut retained_frames)
        });
        self.retained_bytes = retained_bytes;
        self.retained_frames = retained_frames;
        if subs.is_empty() {
            self.subscribers.remove(&id);
            if detach_if_empty {
                self.orphaned_by_delivery.insert(id);
            }
        }
        self.recalculate_retained_totals();
    }

    fn fan_out_to_subscriber(
        sub: &mut ProxySubscriber,
        outbound: &FanOutFrame<'_>,
        retained_bytes: &mut usize,
        retained_frames: &mut usize,
    ) -> bool {
        Self::observe_generation(sub, outbound.frame);
        if matches!(sub.gate, SnapshotGate::Retiring { .. }) {
            return Self::flush_pending_snapshot(sub).1;
        }
        if let FanOutKind::Bootstrap {
            open_after,
            replace_legacy_begin,
        } = outbound.kind
        {
            if replace_legacy_begin {
                sub.gate = SnapshotGate::AwaitingFirst;
            }
            let failure =
                Self::delivery_failure(sub, outbound.host, outbound.terminal, outbound.frame);
            return Self::push_bootstrap_frame(
                sub,
                outbound.frame.clone(),
                open_after,
                failure,
                retained_bytes,
                retained_frames,
            );
        }
        let awaiting_first = matches!(sub.gate, SnapshotGate::AwaitingFirst);
        let (may_send, alive) = Self::flush_pending_snapshot(sub);
        if !alive {
            return false;
        }
        if !may_send {
            if !awaiting_first {
                Self::retain_fan_out_frame(sub, outbound, retained_bytes, retained_frames);
            }
            return true;
        }
        Self::deliver_fan_out_frame(sub, outbound, retained_bytes, retained_frames)
    }

    fn deliver_fan_out_frame(
        sub: &mut ProxySubscriber,
        outbound: &FanOutFrame<'_>,
        retained_bytes: &mut usize,
        retained_frames: &mut usize,
    ) -> bool {
        match sub.out_tx.try_send(Outbound::Frame(outbound.frame.clone())) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                Self::retain_fan_out_frame(sub, outbound, retained_bytes, retained_frames);
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn retain_fan_out_frame(
        sub: &mut ProxySubscriber,
        outbound: &FanOutFrame<'_>,
        retained_bytes: &mut usize,
        retained_frames: &mut usize,
    ) {
        let failure = Self::delivery_failure(sub, outbound.host, outbound.terminal, outbound.frame);
        Self::retain_ordered_frame(
            sub,
            outbound.frame.clone(),
            failure,
            retained_bytes,
            retained_frames,
        );
    }

    fn push_bootstrap_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        open_after: bool,
        delivery_failure: DeliveryFailure,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) -> bool {
        let frame_bytes = Self::retained_frame_bytes(&frame);
        let gate = std::mem::replace(&mut sub.gate, SnapshotGate::AwaitingFirst);
        match gate {
            retiring @ SnapshotGate::Retiring { .. } => {
                sub.gate = retiring;
                true
            }
            SnapshotGate::Retained {
                frames,
                retained_bytes,
                open_after: prior_open_after,
            } => Self::retain_bootstrap_frame(
                sub,
                frame,
                frames,
                retained_bytes,
                prior_open_after || open_after,
                delivery_failure,
                connection_bytes,
                connection_frames,
            ),
            SnapshotGate::AwaitingFirst | SnapshotGate::Open => Self::deliver_bootstrap_frame(
                sub,
                frame,
                frame_bytes,
                open_after,
                delivery_failure,
                connection_bytes,
                connection_frames,
            ),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "retained bootstrap state and both aggregate counters move together atomically"
    )]
    fn retain_bootstrap_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        mut frames: VecDeque<FrameKind>,
        retained_bytes: usize,
        open_after: bool,
        delivery_failure: DeliveryFailure,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) -> bool {
        let frame_bytes = Self::retained_frame_bytes(&frame);
        if Self::retention_exceeded(
            frame_bytes,
            retained_bytes,
            frames.len(),
            *connection_bytes,
            *connection_frames,
        ) {
            *connection_bytes = connection_bytes.saturating_sub(retained_bytes);
            *connection_frames = connection_frames.saturating_sub(frames.len());
            warn!(client = ?sub.client, "relay subscriber exceeded retained delivery bounds; scheduling explicit failure");
            Self::begin_retirement(sub, delivery_failure);
            return true;
        }
        frames.push_back(frame);
        *connection_bytes += frame_bytes;
        *connection_frames += 1;
        sub.gate = SnapshotGate::Retained {
            frames,
            retained_bytes: retained_bytes.saturating_add(frame_bytes),
            open_after,
        };
        true
    }

    fn deliver_bootstrap_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        frame_bytes: usize,
        open_after: bool,
        delivery_failure: DeliveryFailure,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) -> bool {
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
                if Self::retention_exceeded(
                    frame_bytes,
                    0,
                    0,
                    *connection_bytes,
                    *connection_frames,
                ) {
                    Self::begin_retirement(sub, delivery_failure);
                    return true;
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

    const fn retention_exceeded(
        frame_bytes: usize,
        retained_bytes: usize,
        retained_frames: usize,
        connection_bytes: usize,
        connection_frames: usize,
    ) -> bool {
        retained_bytes.saturating_add(frame_bytes) > MAX_RELAY_SUBSCRIBER_RETAINED_BYTES
            || retained_frames.saturating_add(1) > MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES
            || connection_bytes.saturating_add(frame_bytes) > MAX_RELAY_CONNECTION_RETAINED_BYTES
            || connection_frames.saturating_add(1) > MAX_RELAY_CONNECTION_RETAINED_FRAMES
    }

    fn retained_frame_bytes(frame: &FrameKind) -> usize {
        let dynamic = match frame {
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } => Self::resource_id_bytes(terminal_id).saturating_add(bytes.len()),
            FrameKind::BootstrapChunk {
                terminal_id,
                payload,
                ..
            } => Self::resource_id_bytes(terminal_id).saturating_add(payload.len()),
            FrameKind::BootstrapReady {
                terminal_id,
                history_cursor,
                ..
            } => Self::resource_id_bytes(terminal_id)
                .saturating_add(history_cursor.as_ref().map_or(0, bytes::Bytes::len)),
            FrameKind::HistoryPage {
                terminal_id,
                cursor,
                next_cursor,
                payload,
                ..
            } => Self::resource_id_bytes(terminal_id)
                .saturating_add(cursor.len())
                .saturating_add(next_cursor.as_ref().map_or(0, bytes::Bytes::len))
                .saturating_add(payload.len()),
            FrameKind::HistoryTombstone {
                terminal_id,
                cursor,
                ..
            }
            | FrameKind::HistoryRejected {
                terminal_id,
                cursor,
                ..
            } => Self::resource_id_bytes(terminal_id).saturating_add(cursor.len()),
            FrameKind::Event {
                terminal, event, ..
            } => terminal
                .as_ref()
                .map_or(0, Self::resource_id_bytes)
                .saturating_add(Self::agent_event_bytes(event)),
            FrameKind::BootstrapBegin { terminal_id, .. }
            | FrameKind::BootstrapTombstone { terminal_id, .. }
            | FrameKind::ResourceClosed { terminal_id, .. }
            | FrameKind::Bell { terminal_id } => Self::resource_id_bytes(terminal_id),
            _ => 0,
        };
        dynamic.saturating_add(RETAINED_FRAME_OVERHEAD)
    }

    fn resource_id_bytes(id: &ResourceId) -> usize {
        match id {
            ResourceId::Local { .. } => 0,
            ResourceId::Satellite { host, .. } => host.as_str().len(),
        }
    }

    fn agent_event_bytes(event: &AgentEvent) -> usize {
        match event {
            AgentEvent::TitleChanged { title } => title.capacity(),
            AgentEvent::ResourceSpawned { parent, .. } => {
                parent.as_ref().map_or(0, Self::resource_id_bytes)
            }
            AgentEvent::Asked {
                id,
                question,
                suggestions,
                ..
            } => id
                .capacity()
                .saturating_add(question.capacity())
                .saturating_add(
                    suggestions
                        .capacity()
                        .saturating_mul(std::mem::size_of::<String>()),
                )
                .saturating_add(
                    suggestions
                        .iter()
                        .map(String::capacity)
                        .fold(0usize, usize::saturating_add),
                ),
            AgentEvent::CwdChanged { cwd } => cwd.capacity(),
            AgentEvent::Unknown { body, .. } => body.capacity(),
            _ => 0,
        }
    }

    fn delivery_failure(
        sub: &ProxySubscriber,
        host: &SatelliteHost,
        terminal: u32,
        frame: &FrameKind,
    ) -> DeliveryFailure {
        if let FrameKind::HistoryPage {
            stream_id,
            bootstrap_id,
            cursor,
            ..
        } = frame
        {
            return DeliveryFailure {
                frame: FrameKind::HistoryTombstone {
                    terminal_id: ResourceId::satellite(host.clone(), terminal),
                    stream_id: *stream_id,
                    bootstrap_id: *bootstrap_id,
                    cursor: cursor.clone(),
                    reason: HistoryTombstoneReason::Limit,
                },
                detach_upstream: true,
            };
        }
        let identity = match frame {
            FrameKind::ResourceOutput {
                stream_id,
                bootstrap_id,
                seq,
                ..
            } => Some((*stream_id, *bootstrap_id, seq.saturating_sub(1))),
            FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                base_seq,
                ..
            } => Some((*stream_id, *bootstrap_id, *base_seq)),
            FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            } => Some((
                *stream_id,
                *bootstrap_id,
                Self::last_valid_seq(sub, *stream_id, *bootstrap_id),
            )),
            _ => sub.current_generation,
        };
        let failure = identity.map_or_else(
            || FrameKind::Error {
                request_id: None,
                code: ErrorCode::ResourceExhausted,
                message: format!(
                    "satellite {host} terminal {terminal} relay delivery exceeded bounded downstream capacity; subscription closed"
                ),
            },
            |(stream_id, bootstrap_id, last_valid_seq)| FrameKind::BootstrapTombstone {
                terminal_id: ResourceId::satellite(host.clone(), terminal),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::OutboundGap,
                last_valid_seq,
            },
        );
        DeliveryFailure {
            frame: failure,
            detach_upstream: !matches!(frame, FrameKind::ResourceClosed { .. }),
        }
    }

    fn begin_retirement(sub: &mut ProxySubscriber, failure: DeliveryFailure) {
        sub.gate = SnapshotGate::Retiring {
            failure: failure.frame,
            deadline: std::time::Instant::now() + RELAY_RETIREMENT_TIMEOUT,
            detach_upstream: failure.detach_upstream,
        };
    }

    fn observe_generation(sub: &mut ProxySubscriber, frame: &FrameKind) {
        let generation = match frame {
            FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                base_seq,
                ..
            } => Some((*stream_id, *bootstrap_id, *base_seq)),
            FrameKind::ResourceOutput {
                stream_id,
                bootstrap_id,
                seq,
                ..
            }
            | FrameKind::BootstrapTombstone {
                stream_id,
                bootstrap_id,
                last_valid_seq: seq,
                ..
            } => Some((*stream_id, *bootstrap_id, *seq)),
            FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryPage {
                stream_id,
                bootstrap_id,
                ..
            } => Some((
                *stream_id,
                *bootstrap_id,
                Self::last_valid_seq(sub, *stream_id, *bootstrap_id),
            )),
            _ => None,
        };
        if let Some(generation) = generation {
            sub.current_generation = Some(generation);
        }
    }

    fn last_valid_seq(sub: &ProxySubscriber, stream: StreamId, bootstrap: BootstrapId) -> u64 {
        sub.current_generation.map_or(0, |current| {
            if current.0 == stream && current.1 == bootstrap {
                current.2
            } else {
                0
            }
        })
    }

    const fn detach_upstream_on_removal(sub: &ProxySubscriber) -> bool {
        match &sub.gate {
            SnapshotGate::Retiring {
                detach_upstream, ..
            } => *detach_upstream,
            _ => !sub.retire_after_flush,
        }
    }

    fn retain_ordered_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        delivery_failure: DeliveryFailure,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) {
        let frame_bytes = Self::retained_frame_bytes(&frame);
        let gate = std::mem::replace(&mut sub.gate, SnapshotGate::AwaitingFirst);
        let (mut frames, retained_bytes, open_after) = match gate {
            SnapshotGate::Retained {
                frames,
                retained_bytes,
                open_after,
            } => (frames, retained_bytes, open_after),
            SnapshotGate::Open => (VecDeque::new(), 0, true),
            SnapshotGate::AwaitingFirst => (VecDeque::new(), 0, false),
            SnapshotGate::Retiring {
                failure,
                deadline,
                detach_upstream,
            } => {
                if matches!(&frame, FrameKind::ResourceClosed { .. }) {
                    // The upstream close supersedes a queued delivery-gap
                    // fence (and its redundant detach).
                    sub.gate = SnapshotGate::Retiring {
                        failure: frame,
                        deadline,
                        detach_upstream: false,
                    };
                } else {
                    sub.gate = SnapshotGate::Retiring {
                        failure,
                        deadline,
                        detach_upstream,
                    };
                }
                return;
            }
        };
        let next_bytes = retained_bytes.saturating_add(frame_bytes);
        let next_frames = frames.len().saturating_add(1);
        if next_bytes > MAX_RELAY_SUBSCRIBER_RETAINED_BYTES
            || next_frames > MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES
            || connection_bytes.saturating_add(frame_bytes) > MAX_RELAY_CONNECTION_RETAINED_BYTES
            || connection_frames.saturating_add(1) > MAX_RELAY_CONNECTION_RETAINED_FRAMES
        {
            *connection_bytes = connection_bytes.saturating_sub(retained_bytes);
            *connection_frames = connection_frames.saturating_sub(frames.len());
            Self::begin_retirement(sub, delivery_failure);
            return;
        }
        frames.push_back(frame);
        *connection_bytes = connection_bytes.saturating_add(frame_bytes);
        *connection_frames = connection_frames.saturating_add(1);
        sub.gate = SnapshotGate::Retained {
            frames,
            retained_bytes: next_bytes,
            open_after,
        };
    }

    /// Best-effort delivery past the snapshot gate for frames the snapshot
    /// does not capture (`RESOURCE_CLOSED`, `BELL`). A refusal reaps the
    /// subscriber.
    fn fan_out_ungated(&mut self, id: u32, frame: &FrameKind) {
        let host = &self.host;
        let Some(subs) = self.subscribers.get_mut(&id) else {
            trace!(satellite = %self.host, terminal = id, "ungated frame with no proxy subscribers");
            return;
        };
        let detach_if_empty = subs.iter().any(Self::detach_upstream_on_removal);
        let mut retained_bytes = self.retained_bytes;
        let mut retained_frames = self.retained_frames;
        subs.retain_mut(|sub| {
            Self::observe_generation(sub, frame);
            if matches!(
                sub.gate,
                SnapshotGate::Retained { .. } | SnapshotGate::Retiring { .. }
            ) {
                let failure = Self::delivery_failure(sub, host, id, frame);
                Self::retain_ordered_frame(
                    sub,
                    frame.clone(),
                    failure,
                    &mut retained_bytes,
                    &mut retained_frames,
                );
                return true;
            }
            match sub.out_tx.try_send(Outbound::Frame(frame.clone())) {
                Ok(()) => !sub.retire_after_flush,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let failure = Self::delivery_failure(sub, host, id, frame);
                    Self::retain_ordered_frame(
                        sub,
                        frame.clone(),
                        failure,
                        &mut retained_bytes,
                        &mut retained_frames,
                    );
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
        self.retained_bytes = retained_bytes;
        self.retained_frames = retained_frames;
        if subs.is_empty() {
            self.subscribers.remove(&id);
            if detach_if_empty {
                self.orphaned_by_delivery.insert(id);
            }
        }
        self.recalculate_retained_totals();
    }

    /// Retry one subscriber's retained bootstrap prefix. Returns
    /// `(may_send_delta, subscriber_alive)`.
    fn flush_pending_snapshot(sub: &mut ProxySubscriber) -> (bool, bool) {
        let gate = std::mem::replace(&mut sub.gate, SnapshotGate::AwaitingFirst);
        if let SnapshotGate::Retiring {
            failure,
            deadline,
            detach_upstream,
        } = gate
        {
            return match sub.out_tx.try_send(Outbound::Frame(failure.clone())) {
                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => (false, false),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    if std::time::Instant::now() >= deadline {
                        // Other clones keep the mailbox alive; cancel the
                        // connection itself.
                        sub.consumer_cancel.cancel();
                        (false, false)
                    } else {
                        sub.gate = SnapshotGate::Retiring {
                            failure,
                            deadline,
                            detach_upstream,
                        };
                        (false, true)
                    }
                }
            };
        }
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
        (open_after, !sub.retire_after_flush)
    }

    /// Retry every retained snapshot (keepalive tick), for terminals with no
    /// further traffic to trigger the inline retry.
    pub(crate) fn flush_pending_snapshots(&mut self) {
        let mut orphaned = Vec::new();
        for (terminal, subs) in &mut self.subscribers {
            let detach_if_empty = subs.iter().any(Self::detach_upstream_on_removal);
            subs.retain_mut(|sub| {
                if matches!(
                    sub.gate,
                    SnapshotGate::Retained { .. } | SnapshotGate::Retiring { .. }
                ) {
                    Self::flush_pending_snapshot(sub).1
                } else {
                    true
                }
            });
            if subs.is_empty() && detach_if_empty {
                orphaned.push(*terminal);
            }
        }
        self.subscribers.retain(|_, subs| !subs.is_empty());
        self.orphaned_by_delivery.extend(orphaned);
        self.recalculate_retained_totals();
    }

    pub(crate) fn take_delivery_detaches(&mut self) -> Vec<Vec<u8>> {
        let orphaned = std::mem::take(&mut self.orphaned_by_delivery)
            .into_iter()
            .filter(|terminal| !self.subscribers.contains_key(terminal))
            .collect();
        self.encode_withdrawals(orphaned)
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

    /// Next link-side request id, skipping ids still pending (wrap safety).
    fn allocate_request_id(&mut self) -> u32 {
        loop {
            let id = self.next_request_id;
            self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
            if !self.request_id_in_use(id) {
                return id;
            }
        }
    }

    /// Whether any reply map still holds the link-side id `id`.
    fn request_id_in_use(&self, id: u32) -> bool {
        self.pending.contains_key(&id)
            || self.pending_spawns.contains_key(&id)
            || self.pending_listings.contains_key(&id)
            || self.pending_paths.contains_key(&id)
            || self.pending_detaches.contains_key(&id)
            || self.pending_mirror_gets.contains_key(&id)
    }

    /// Store one allowlisted `METADATA_CHANGED`, retagged to `Satellite`,
    /// for a terminal this link asked to mirror. The satellite chooses the
    /// ids it reports, so an unrequested one would let it grow the hub's
    /// store without bound.
    fn apply_mirrored_metadata(&self, scope: &Scope, key: &str, value: Option<Vec<u8>>) {
        let Scope::Resource(ResourceId::Local { id }) = scope else {
            return;
        };
        if !self.mirrored.contains(id) {
            return;
        }
        let Some(journal) = &self.journal else {
            return;
        };
        let journal = journal.clone();
        let host = self.host.clone();
        journal.with_mut(|state| {
            crate::hub::metadata_mirror::apply_mirrored(state, &host, scope, key, value);
        });
    }

    /// Store one allowlisted `METADATA_VALUE` correlated to a mirror GET.
    fn apply_mirrored_value(&mut self, request_id: u32, value: Option<Vec<u8>>) {
        let Some((terminal, key)) = self.pending_mirror_gets.remove(&request_id) else {
            return;
        };
        self.apply_mirrored_metadata(&Scope::Resource(ResourceId::local(terminal)), &key, value);
    }

    /// Drop the mirror for a satellite terminal that closed.
    fn forget_mirror(&mut self, terminal: u32) {
        self.mirrored.remove(&terminal);
        self.pending_mirror_gets
            .retain(|_, (id, _)| *id != terminal);
        let Some(journal) = &self.journal else {
            return;
        };
        let journal = journal.clone();
        let host = self.host.clone();
        journal.with_mut(|state| {
            crate::hub::metadata_mirror::forget_mirrored_terminal(state, &host, terminal);
            state.forget_satellite_lease_terminal(&host, terminal);
        });
    }

    fn encode(&mut self, frame: &FrameKind) -> Vec<u8> {
        self.encode_buf.clear();
        frame.encode(&mut self.encode_buf);
        self.encode_buf.to_vec()
    }
}

/// Rewrite `KILL_RESOURCE`/`KILL_RESOURCE_IF` to the satellite's `Local`
/// id; precondition and `operation_id` cross verbatim (ADR-0109, L1 §9.1).
fn route_kill_to_satellite(command: &Command) -> Option<(SatelliteHost, Command)> {
    match command {
        Command::KillResource {
            terminal_id,
            operation_id,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::KillResource {
                    terminal_id: ResourceId::local(id),
                    operation_id: *operation_id,
                },
            ))
        }
        Command::KillResourceIf {
            terminal_id,
            precondition,
            operation_id,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::KillResourceIf {
                    terminal_id: ResourceId::local(id),
                    precondition: *precondition,
                    operation_id: *operation_id,
                },
            ))
        }
        _ => None,
    }
}

/// Rewrite `APPLY_INPUT` to the satellite's `Local` id (L1 §9.1).
fn route_input_to_satellite(command: &Command) -> Option<(SatelliteHost, Command)> {
    let Command::ApplyInput {
        operation_id,
        terminal_id,
        events,
    } = command
    else {
        return None;
    };
    let (host, id) = satellite_route(terminal_id)?;
    Some((
        host,
        Command::ApplyInput {
            operation_id: *operation_id,
            terminal_id: ResourceId::local(id),
            events: events.clone(),
        },
    ))
}

/// Re-tag a satellite's spawn reply: `Local { id }` becomes
/// `Satellite { host, id }`, the instance token passes through (ADR-0109),
/// and a `Satellite`-tagged id becomes `SpawnFailed`.
fn retag_spawn_result(host: &SatelliteHost, result: SpawnResult) -> SpawnResult {
    let Some(spawned) = result.spawned_id() else {
        return result;
    };
    let ResourceId::Local { id } = spawned else {
        warn!(
            satellite = %host,
            "satellite answered a spawn with a Satellite-tagged id; hub-and-spoke does not chain"
        );
        return SpawnResult::Err(SpawnError::SpawnFailed(
            "satellite returned a chained satellite id".to_owned(),
        ));
    };
    let id = ResourceId::satellite(host.clone(), *id);
    // A replayed reply stays replayed (ADR-0126).
    match (result.is_replayed(), result.instance()) {
        (true, instance) => SpawnResult::Replayed { id, instance },
        (false, Some(instance)) => SpawnResult::OkBound { id, instance },
        (false, None) => SpawnResult::Ok(id),
    }
}

/// The high bit a [`foreign_client`] id carries. Hub consumers' wire ids are
/// allocated upward from 1 and never reach it.
const FOREIGN_CLIENT_BIT: u32 = 1 << 31;

/// A client id a satellite reported (one of its own connections, often the
/// hub's link itself), moved into a range hub client ids never reach, so it
/// can neither collide with nor impersonate a hub consumer.
pub(crate) const fn foreign_client(
    id: phux_protocol::ids::ClientId,
) -> phux_protocol::ids::ClientId {
    phux_protocol::ids::ClientId::new(FOREIGN_CLIENT_BIT | id.get())
}

/// Split a satellite-tagged wire id into its host and satellite-local id.
pub(crate) fn satellite_route(terminal_id: &ResourceId) -> Option<(SatelliteHost, u32)> {
    match terminal_id {
        ResourceId::Satellite { host, id } => Some((host.clone(), *id)),
        ResourceId::Local { .. } => None,
    }
}

/// The command a hub's link sends for a consumer's attach (ADR-0127).
///
/// The link is one identity shared by every consumer, so only a deliberate
/// takeover crosses, and only to a satellite advertising `ATTACH_ROLES`.
pub(crate) fn link_attach_command(command: Command, satellite: ServerFeatureSet) -> Command {
    let Command::AttachResource {
        terminal_id,
        role_policy,
    } = command
    else {
        return command;
    };
    let role_policy = role_policy
        .filter(|policy| policy.takes_over() && satellite.contains(ServerFeature::AttachRoles));
    Command::AttachResource {
        terminal_id,
        role_policy,
    }
}

/// If `command` targets one satellite-owned terminal, the host and the
/// command rewritten to `Local` ids. `None` for local targets, hub-local
/// commands, and mixed batches.
#[allow(
    clippy::too_many_lines,
    reason = "one mechanical rewrite arm per per-terminal Command variant; splitting hides the catalog"
)]
pub(crate) fn route_to_satellite(command: &Command) -> Option<(SatelliteHost, Command)> {
    match command {
        Command::AttachResource {
            terminal_id,
            role_policy,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            // The consumer's intent rides unchanged; the link command is
            // decided at dispatch (ADR-0127).
            Some((
                host,
                Command::AttachResource {
                    terminal_id: ResourceId::local(id),
                    role_policy: *role_policy,
                },
            ))
        }
        Command::DetachResource { terminal_id } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::DetachResource {
                    terminal_id: ResourceId::local(id),
                },
            ))
        }
        Command::KillResource { .. } | Command::KillResourceIf { .. } => {
            route_kill_to_satellite(command)
        }
        Command::ApplyInput { .. } => route_input_to_satellite(command),
        Command::GetScreen {
            terminal_id,
            request_scrollback,
            cells,
            format,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::GetScreen {
                    terminal_id: ResourceId::local(id),
                    request_scrollback: *request_scrollback,
                    cells: *cells,
                    format: *format,
                },
            ))
        }
        Command::RouteInput { terminal_id, event } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::RouteInput {
                    terminal_id: ResourceId::local(id),
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
                    terminal_id: ResourceId::local(id),
                    include_scrollback: *include_scrollback,
                    max_scrollback_lines: *max_scrollback_lines,
                },
            ))
        }
        Command::SubscribeResourceEvents {
            terminal_id,
            event_types,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::SubscribeResourceEvents {
                    terminal_id: ResourceId::local(id),
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
                    terminal_id: ResourceId::local(id),
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
                    terminal_id: ResourceId::local(id),
                },
            ))
        }
        Command::SignalTerminal {
            terminal_id,
            signal,
            operation_id,
        } => {
            let (host, id) = satellite_route(terminal_id)?;
            Some((
                host,
                Command::SignalTerminal {
                    terminal_id: ResourceId::local(id),
                    signal: *signal,
                    operation_id: *operation_id,
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
                    terminal_id: ResourceId::local(id),
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
                    terminal_id: ResourceId::local(id),
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
                    terminal_id: ResourceId::local(id),
                    state: *state,
                },
            ))
        }
        // Hub-local, batch-partitioned, or unknown commands.
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

    /// Register a `SUBSCRIBE_EVENTS`-shaped subscription at token 1.
    fn subscribe(
        session: &mut RelaySession,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) {
        subscribe_at(session, terminal, client, 1, out_tx);
    }

    /// [`subscribe`] with an explicit issue-order token.
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
                consumer_cancel: CancellationToken::new(),
                seq,
                awaits_snapshot: false,
                bootstrap_profile: None,
                bootstrap_limits: None,
            },
            forward: FrameKind::SubscribeEvents {
                terminal: Some(ResourceId::local(terminal)),
                after_seq: None,
            },
        });
        assert!(
            !wire.is_empty(),
            "atomic subscribe must produce the forward frame"
        );
    }

    /// Register an `ATTACH_RESOURCE` subscription (starts gated).
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
            command: Command::AttachResource {
                terminal_id: ResourceId::local(terminal),
                role_policy: None,
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal,
                client,
                out_tx,
                consumer_cancel: CancellationToken::new(),
                seq: 1,
                awaits_snapshot: true,
                bootstrap_profile: Some(selected_profile),
                bootstrap_limits: Some(selected_limits),
            }),
        });
        assert!(!wire.is_empty(), "attach must produce the COMMAND frame");
    }

    /// ADR-0136 mirror bounds: a satellite writes the hub's copy only for a
    /// terminal the hub asked to mirror, and never past the hub's metadata
    /// value cap.
    #[test]
    fn a_satellite_writes_only_requested_bounded_mirror_entries() {
        use phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
        let state = crate::state::SharedState::new();
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        session.set_journal(Some(state.clone()));
        let changed = |id: u32, value: Vec<u8>| {
            encode(&FrameKind::MetadataChanged {
                scope: Scope::Resource(ResourceId::local(id)),
                key: RESOURCE_AGENT_KEY.to_owned(),
                value: Some(value),
                actor: None,
            })
        };
        let stored = |id: u32| {
            state.with(|s| {
                s.metadata().get(
                    &Scope::Resource(ResourceId::satellite(host(), id)),
                    RESOURCE_AGENT_KEY,
                )
            })
        };

        session.handle_inbound(&changed(7, b"{}".to_vec())).unwrap();
        assert!(stored(7).is_none(), "the hub never asked to mirror 7");

        let _ = session.prepare_request(&RelayRequest::MirrorTerminal { terminal: 7 });
        session.handle_inbound(&changed(7, b"{}".to_vec())).unwrap();
        assert_eq!(stored(7).as_deref(), Some(b"{}".as_slice()));

        let cap = state.with(crate::state::ServerState::metadata_value_bytes) as usize;
        session
            .handle_inbound(&changed(7, vec![b'x'; cap + 1]))
            .unwrap();
        assert!(stored(7).is_none(), "an oversized value clears the copy");
    }

    /// A satellite terminal's lease-mirror ordering state is dropped when
    /// it closes, so terminals the satellite names come and go without
    /// growing the hub.
    #[test]
    fn a_closed_satellite_terminal_leaves_no_lease_mirror_state() {
        use phux_protocol::wire::frame::CloseReason;
        let state = crate::state::SharedState::new();
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        session.set_journal(Some(state.clone()));
        for id in 0..64 {
            state.with_mut(|s| {
                s.mirror_satellite_lease_event(&host(), id, false, Some(1));
            });
            session
                .handle_inbound(&encode(&FrameKind::ResourceClosed {
                    terminal_id: ResourceId::local(id),
                    exit_status: None,
                    reason: CloseReason::Exited,
                    signal: None,
                }))
                .unwrap();
        }
        assert_eq!(
            state.with(crate::state::ServerState::satellite_lease_mirror_entries),
            0
        );
    }

    // --- outbound command rewrite ---------------------------------------

    /// ADR-0127: only a takeover crosses, and only with `ATTACH_ROLES`.
    #[test]
    fn hub_relays_role_policy_to_the_satellite_when_it_advertises_attach_roles() {
        use phux_protocol::wire::frame::RolePolicy;
        let attach = |role_policy| Command::AttachResource {
            terminal_id: ResourceId::local(3),
            role_policy,
        };
        let roles = ServerFeatureSet::with(&[ServerFeature::AttachRoles]);
        let takeover = Some(RolePolicy::TAKEOVER);
        assert_eq!(
            link_attach_command(attach(takeover), roles),
            attach(takeover)
        );
        assert_eq!(
            link_attach_command(attach(takeover), ServerFeatureSet::new()),
            attach(None),
            "a satellite without the bit gets a plain attach; the hub seizes after it",
        );
        for held in [RolePolicy::VIEWER, RolePolicy::PRIMARY] {
            assert_eq!(link_attach_command(attach(Some(held)), roles), attach(None));
        }
        let detach = Command::DetachResource {
            terminal_id: ResourceId::local(3),
        };
        assert_eq!(link_attach_command(detach.clone(), roles), detach);
    }

    #[test]
    fn route_to_satellite_rewrites_terminal_ids_to_local() {
        let command = Command::GetScreen {
            terminal_id: ResourceId::satellite("devbox", 7),
            request_scrollback: Some(10),
            cells: true,
            format: 0,
        };
        let (routed_host, rewritten) = route_to_satellite(&command).expect("satellite target");
        assert_eq!(routed_host, host());
        assert_eq!(
            rewritten,
            Command::GetScreen {
                terminal_id: ResourceId::local(7),
                request_scrollback: Some(10),
                cells: true,
                format: 0,
            }
        );
    }

    #[test]
    fn route_to_satellite_ignores_local_and_unscoped_commands() {
        assert!(
            route_to_satellite(&Command::GetScreen {
                terminal_id: ResourceId::local(7),
                request_scrollback: None,
                cells: false,
                format: 0,
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
            route_to_satellite(&Command::KillResources {
                ids: vec![ResourceId::satellite("devbox", 1)],
                operation_id: None,
            })
            .is_none()
        );
        assert!(
            route_to_satellite(&Command::CloseTabResources {
                ids: vec![ResourceId::satellite("devbox", 1)],
            })
            .is_none()
        );
    }

    #[test]
    fn route_to_satellite_covers_every_per_terminal_command() {
        let sat = ResourceId::satellite("devbox", 3);
        let commands = [
            Command::AttachResource {
                terminal_id: sat.clone(),
                role_policy: None,
            },
            Command::DetachResource {
                terminal_id: sat.clone(),
            },
            Command::KillResource {
                terminal_id: sat.clone(),
                operation_id: None,
            },
            Command::GetTerminalState {
                terminal_id: sat.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
            Command::SubscribeResourceEvents {
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
                operation_id: None,
            },
            Command::ReportAsked {
                terminal_id: sat.clone(),
                id: "q".to_owned(),
                question: "?".to_owned(),
                suggestions: vec![],
                elapsed_seconds: None,
            },
            Command::ReportAgentState {
                terminal_id: sat.clone(),
                state: phux_protocol::wire::frame::ReportedAgentState::Done,
            },
            Command::ApplyInput {
                operation_id: phux_protocol::InputOperationId::new([1; 16]).expect("id"),
                terminal_id: sat.clone(),
                events: vec![],
            },
            Command::KillResourceIf {
                terminal_id: sat,
                precondition: phux_protocol::wire::frame::KillPrecondition::spawned_and_unattached(
                    phux_protocol::ids::ServerInstance::new([3; 16]),
                ),
                operation_id: None,
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
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            native_profile(),
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        let (out_tx, mut out_rx) = mpsc::channel(8);
        attach(&mut session, 7, ClientId(1), out_tx);
        let error = session
            .handle_inbound(&encode(&FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(7),
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

        let mut gapped = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            native_profile(),
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        gapped
            .handle_inbound(&encode(&FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(7),
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
                terminal_id: ResourceId::local(7),
                stream_id,
                bootstrap_id,
                chunk_seq: 1,
                payload: bytes::Bytes::from_static(b"opaque"),
            }))
            .expect_err("gapped chunk must fail the link");
        assert!(error.contains("expected 0"));

        let mut early_live = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            native_profile(),
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        early_live
            .handle_inbound(&encode(&FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(7),
                stream_id,
                bootstrap_id,
                profile: native_stream_profile(),
                cols: 80,
                rows: 24,
                base_seq: 10,
            }))
            .expect("valid BEGIN");
        let error = early_live
            .handle_inbound(&encode(&FrameKind::ResourceOutput {
                terminal_id: ResourceId::local(7),
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
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            native_profile(),
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        let (out_tx, mut out_rx) = mpsc::channel(8);
        attach(&mut session, 7, ClientId(1), out_tx);
        let stream_id = StreamId::new(4).expect("stream");
        let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(7),
                stream_id,
                bootstrap_id,
                profile: native_stream_profile(),
                cols: 80,
                rows: 24,
                base_seq: 20,
            },
            FrameKind::BootstrapChunk {
                terminal_id: ResourceId::local(7),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"opaque"),
            },
            FrameKind::BootstrapReady {
                terminal_id: ResourceId::local(7),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            FrameKind::ResourceOutput {
                terminal_id: ResourceId::local(7),
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
            Outbound::Frame(FrameKind::ResourceOutput { seq: 21, .. })
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
        let mut session = RelaySession::new_negotiated(
            host(),
            limits,
            native_profile(),
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        let (out_tx, _out_rx) = mpsc::channel(8);
        let (reply, mut reply_rx) = oneshot::channel();
        let wire = session.handle_request_checked(RelayRequest::Command {
            command: Command::AttachResource {
                terminal_id: ResourceId::local(7),
                role_policy: None,
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 7,
                client: ClientId(1),
                out_tx,
                consumer_cancel: CancellationToken::new(),
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
            command: Command::AttachResource {
                terminal_id: ResourceId::local(8),
                role_policy: None,
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 8,
                client: ClientId(2),
                out_tx,
                consumer_cancel: CancellationToken::new(),
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
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            native_profile(),
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        let (out_tx, mut out_rx) = mpsc::channel(8);
        attach(&mut session, 7, ClientId(1), out_tx);
        let stream_id = StreamId::new(4).expect("stream");
        let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(7),
                stream_id,
                bootstrap_id,
                profile: native_stream_profile(),
                cols: 80,
                rows: 24,
                base_seq: 20,
            },
            FrameKind::BootstrapReady {
                terminal_id: ResourceId::local(7),
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
            .handle_inbound(&encode(&FrameKind::ResourceOutput {
                terminal_id: ResourceId::local(7),
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
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            native_profile(),
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
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
    fn saturated_subscribers_converge_or_receive_an_explicit_failure() {
        let mut open = RelaySession::new(host(), BootstrapLimits::default());
        let (open_tx, mut open_rx) = mpsc::channel(1);
        subscribe(&mut open, 9, ClientId(1), open_tx.clone());
        open_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill open mailbox");
        open.fan_out(
            9,
            &FrameKind::Event {
                terminal: Some(ResourceId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            },
        );
        let _ = open_rx.try_recv().expect("filler remains");
        open.flush_pending_snapshots();
        assert!(open.subscribers.contains_key(&9));
        assert!(matches!(
            open_rx
                .try_recv()
                .expect("retained event resumes after drain"),
            Outbound::Frame(FrameKind::Event { .. })
        ));

        let mut closed = RelaySession::new(host(), BootstrapLimits::default());
        let (closed_tx, closed_rx) = mpsc::channel(1);
        subscribe(&mut closed, 9, ClientId(1), closed_tx);
        drop(closed_rx);
        closed.fan_out(
            9,
            &FrameKind::Event {
                terminal: Some(ResourceId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            },
        );
        assert!(!closed.subscribers.contains_key(&9));

        let mut retained = RelaySession::new(host(), BootstrapLimits::default());
        let (retained_tx, mut retained_rx) = mpsc::channel(1);
        subscribe(&mut retained, 9, ClientId(1), retained_tx.clone());
        retained_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill retained mailbox");
        for chunk_seq in 0..=MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES {
            retained.fan_out(
                9,
                &FrameKind::BootstrapChunk {
                    terminal_id: ResourceId::satellite("devbox", 9),
                    stream_id: StreamId::new(1).expect("stream"),
                    bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                    chunk_seq: u32::try_from(chunk_seq).expect("small sequence"),
                    payload: bytes::Bytes::from_static(b"x"),
                },
            );
        }
        assert!(retained.subscribers.contains_key(&9));
        assert_eq!(retained.retained_bytes, 0);
        assert_eq!(retained.retained_frames, 0);
        let _ = retained_rx.try_recv().expect("filler remains");
        retained.flush_pending_snapshots();
        assert!(!retained.subscribers.contains_key(&9));
        let Outbound::Frame(FrameKind::BootstrapTombstone {
            terminal_id,
            reason,
            ..
        }) = retained_rx.try_recv().expect("explicit saturation failure")
        else {
            panic!("content saturation must deliver a generation tombstone")
        };
        assert_eq!(terminal_id, ResourceId::satellite("devbox", 9));
        assert_eq!(reason, TombstoneReason::OutboundGap);
        assert_eq!(retained.take_delivery_detaches().len(), 1);
    }

    #[test]
    fn connection_wide_retention_budget_marks_excess_subscriber_for_failure() {
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
                terminal_id: ResourceId::satellite("devbox", 9),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                chunk_seq: 0,
                payload,
            },
        );
        assert_eq!(session.subscribers[&9].len(), 9);
        assert!(session.retained_bytes <= MAX_RELAY_CONNECTION_RETAINED_BYTES);
        assert_eq!(session.retained_frames, 7);
        assert!(matches!(
            session.subscribers[&9][7].gate,
            SnapshotGate::Retiring { .. }
        ));
        assert_eq!(receivers.len(), 9);
    }

    #[test]
    fn live_and_history_payloads_hit_the_byte_limit_before_the_frame_limit() {
        let stream_id = StreamId::new(1).expect("stream");
        let bootstrap_id = BootstrapId::new(1).expect("bootstrap");

        let mut live = RelaySession::new(host(), BootstrapLimits::default());
        let (live_tx, _live_rx) = mpsc::channel(1);
        live_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill live mailbox");
        subscribe(&mut live, 9, ClientId(1), live_tx);
        live.fan_out(
            9,
            &FrameKind::ResourceOutput {
                terminal_id: ResourceId::satellite("devbox", 9),
                stream_id,
                bootstrap_id,
                seq: 8,
                bytes: bytes::Bytes::from(vec![0; MAX_RELAY_SUBSCRIBER_RETAINED_BYTES]),
            },
        );
        let SnapshotGate::Retiring { failure, .. } = &live.subscribers[&9][0].gate else {
            panic!("one legal large live frame must trip the byte bound before the frame bound")
        };
        assert!(matches!(
            failure,
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id: failed_stream,
                bootstrap_id: failed_bootstrap,
                last_valid_seq: 7,
                ..
            } if terminal_id == &ResourceId::satellite("devbox", 9)
                && *failed_stream == stream_id
                && *failed_bootstrap == bootstrap_id
        ));

        let mut history = RelaySession::new(host(), BootstrapLimits::default());
        let (history_tx, mut history_rx) = mpsc::channel(1);
        history_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill history mailbox");
        subscribe(&mut history, 9, ClientId(1), history_tx);
        let cursor = bytes::Bytes::from_static(b"cursor-9");
        history.fan_out(
            9,
            &FrameKind::HistoryPage {
                terminal_id: ResourceId::satellite("devbox", 9),
                stream_id,
                bootstrap_id,
                page_seq: 1,
                cursor: cursor.clone(),
                next_cursor: None,
                payload: bytes::Bytes::from(vec![0; MAX_RELAY_SUBSCRIBER_RETAINED_BYTES]),
                rows: 1,
            },
        );
        let SnapshotGate::Retiring { failure, .. } = &history.subscribers[&9][0].gate else {
            panic!("one legal large history page must trip the byte bound before the frame bound")
        };
        assert!(matches!(
            failure,
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id: failed_stream,
                bootstrap_id: failed_bootstrap,
                cursor: failed_cursor,
                reason: HistoryTombstoneReason::Limit,
            } if terminal_id == &ResourceId::satellite("devbox", 9)
                && *failed_stream == stream_id
                && *failed_bootstrap == bootstrap_id
                && failed_cursor == &cursor
        ));
        let _ = history_rx.try_recv().expect("filler");
        history.flush_pending_snapshots();
        assert!(matches!(
            history_rx.try_recv().expect("history fence"),
            Outbound::Frame(FrameKind::HistoryTombstone { .. })
        ));
        assert!(!history.subscribers.contains_key(&9));
        assert_eq!(history.take_delivery_detaches().len(), 1);
    }

    #[test]
    fn permanently_full_retirement_cancels_consumer_and_detaches_upstream() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(1);
        let production_owner = out_tx.clone();
        let consumer_cancel = CancellationToken::new();
        out_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill downstream mailbox");
        let wire = session.handle_request(RelayRequest::Subscribe {
            subscription: ProxySubscription {
                terminal: 9,
                client: ClientId(1),
                out_tx,
                consumer_cancel: consumer_cancel.clone(),
                seq: 1,
                awaits_snapshot: false,
                bootstrap_profile: None,
                bootstrap_limits: None,
            },
            forward: FrameKind::SubscribeEvents {
                terminal: Some(ResourceId::local(9)),
                after_seq: None,
            },
        });
        assert!(!wire.is_empty());
        for chunk_seq in 0..=MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES {
            session.fan_out(
                9,
                &FrameKind::BootstrapChunk {
                    terminal_id: ResourceId::satellite("devbox", 9),
                    stream_id: StreamId::new(1).expect("stream"),
                    bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                    chunk_seq: u32::try_from(chunk_seq).expect("small sequence"),
                    payload: bytes::Bytes::from_static(b"x"),
                },
            );
        }
        let SnapshotGate::Retiring { deadline, .. } =
            &mut session.subscribers.get_mut(&9).expect("subscriber")[0].gate
        else {
            panic!("overflow starts bounded retirement")
        };
        *deadline = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(1))
            .expect("one millisecond is representable");

        session.flush_pending_snapshots();
        assert!(!session.subscribers.contains_key(&9));
        assert_eq!(session.take_delivery_detaches().len(), 1);
        assert!(consumer_cancel.is_cancelled());
        let _ = out_rx.try_recv().expect("queued filler");
        assert!(matches!(
            out_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(production_owner);
        assert!(matches!(
            out_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
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
        let FrameKind::SpawnResource {
            request_id,
            satellite,
            ..
        } = decode(&wire)
        else {
            panic!("expected SPAWN_RESOURCE on the wire");
        };
        assert_eq!(
            satellite, None,
            "the addressing field never crosses the link (no chaining)"
        );
        session
            .handle_inbound(&encode(&FrameKind::ResourceSpawned {
                request_id,
                result: SpawnResult::Ok(ResourceId::local(42)),
            }))
            .expect("valid satellite frame");
        assert_eq!(
            rx.try_recv().expect("spawn resolved"),
            SpawnResult::Ok(ResourceId::satellite("devbox", 42))
        );
    }

    #[test]
    fn session_rejects_chained_ids_and_relays_spawn_errors_verbatim() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        // A Satellite-tagged id in the satellite's own reply never chains.
        let (reply, mut rx) = oneshot::channel();
        let wire = session.handle_request(spawn_request(reply));
        let FrameKind::SpawnResource { request_id, .. } = decode(&wire) else {
            panic!("expected SPAWN_RESOURCE");
        };
        session
            .handle_inbound(&encode(&FrameKind::ResourceSpawned {
                request_id,
                result: SpawnResult::Ok(ResourceId::satellite("nested", 7)),
            }))
            .expect("valid satellite frame");
        assert!(matches!(
            rx.try_recv().expect("resolved"),
            SpawnResult::Err(SpawnError::SpawnFailed(_))
        ));
        // A typed satellite-side error relays verbatim.
        let (reply, mut rx) = oneshot::channel();
        let wire = session.handle_request(spawn_request(reply));
        let FrameKind::SpawnResource { request_id, .. } = decode(&wire) else {
            panic!("expected SPAWN_RESOURCE");
        };
        session
            .handle_inbound(&encode(&FrameKind::ResourceSpawned {
                request_id,
                result: SpawnResult::Err(SpawnError::GroupNotFound),
            }))
            .expect("valid satellite frame");
        assert_eq!(
            rx.try_recv().expect("resolved"),
            SpawnResult::Err(SpawnError::GroupNotFound)
        );
    }

    /// ADR-0109: the instance token comes back beside the re-tagged id.
    #[test]
    fn session_keeps_a_bound_spawn_token_while_retagging_the_id() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, mut rx) = oneshot::channel();
        let mut request = spawn_request(reply);
        if let RelayRequest::Spawn { spawn, .. } = &mut request {
            spawn.resource = Some(Box::new(
                phux_protocol::wire::frame::SpawnResource::default().with_bind_instance(true),
            ));
        }
        let wire = session.handle_request(request);
        let FrameKind::SpawnResource {
            request_id,
            resource,
            ..
        } = decode(&wire)
        else {
            panic!("expected SPAWN_RESOURCE on the wire");
        };
        assert!(
            resource.is_some_and(|r| r.bind_instance),
            "the bind request crosses the link"
        );
        let instance = phux_protocol::ids::ServerInstance::new([3; 16]);
        session
            .handle_inbound(&encode(&FrameKind::ResourceSpawned {
                request_id,
                result: SpawnResult::OkBound {
                    id: ResourceId::local(42),
                    instance,
                },
            }))
            .expect("valid satellite frame");
        assert_eq!(
            rx.try_recv().expect("spawn resolved"),
            SpawnResult::OkBound {
                id: ResourceId::satellite("devbox", 42),
                instance,
            }
        );
    }

    /// ADR-0126: a replayed reply stays replayed through the hub.
    #[test]
    fn session_keeps_a_replayed_spawn_replayed_while_retagging_the_id() {
        let key = phux_protocol::ids::IdempotencyKey::new([7; 16]);
        let instance = phux_protocol::ids::ServerInstance::new([3; 16]);
        for original in [None, Some(instance)] {
            let mut session = RelaySession::new_negotiated(
                host(),
                BootstrapLimits::default(),
                BootstrapProfile::SynthesizedVtRaw,
                ServerFeatureSet::with(&[ServerFeature::SpawnIdempotency]),
            );
            let (reply, mut rx) = oneshot::channel();
            let mut request = spawn_request(reply);
            if let RelayRequest::Spawn { spawn, .. } = &mut request {
                spawn.resource = Some(Box::new(
                    phux_protocol::wire::frame::SpawnResource::default().with_idempotency_key(key),
                ));
            }
            let wire = session.handle_request(request);
            let FrameKind::SpawnResource {
                request_id,
                resource,
                ..
            } = decode(&wire)
            else {
                panic!("expected SPAWN_RESOURCE on the wire");
            };
            assert_eq!(
                resource.and_then(|r| r.idempotency_key),
                key,
                "the key crosses the link"
            );
            session
                .handle_inbound(&encode(&FrameKind::ResourceSpawned {
                    request_id,
                    result: SpawnResult::Replayed {
                        id: ResourceId::local(42),
                        instance: original,
                    },
                }))
                .expect("valid satellite frame");
            assert_eq!(
                rx.try_recv().expect("spawn resolved"),
                SpawnResult::Replayed {
                    id: ResourceId::satellite("devbox", 42),
                    instance: original,
                }
            );
        }
    }

    /// ADR-0124: `retain_secs` crosses only with `RETAIN_ON_EXIT`; the hub
    /// refuses otherwise. A spawn without it still relays.
    #[test]
    fn hub_forwards_retain_secs_only_to_a_satellite_that_honors_it() {
        let mut capable = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::with(&[ServerFeature::RetainOnExit]),
        );
        let (reply, _rx) = oneshot::channel();
        let mut request = spawn_request(reply);
        if let RelayRequest::Spawn { spawn, .. } = &mut request {
            spawn.resource = Some(Box::new(
                phux_protocol::wire::frame::SpawnResource::default().with_retain_secs(Some(600)),
            ));
        }
        let wire = capable
            .handle_request_checked(request)
            .expect("relayed to a satellite that honors retain_secs");
        let FrameKind::SpawnResource { resource, .. } = decode(&wire) else {
            panic!("expected SPAWN_RESOURCE on the wire");
        };
        assert_eq!(
            resource.and_then(|r| r.retain_secs),
            Some(600),
            "retain_secs crosses the link"
        );

        let mut older = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, mut rx) = oneshot::channel();
        let mut request = spawn_request(reply);
        if let RelayRequest::Spawn { spawn, .. } = &mut request {
            spawn.resource = Some(Box::new(
                phux_protocol::wire::frame::SpawnResource::default().with_retain_secs(Some(600)),
            ));
        }
        assert!(
            older.handle_request_checked(request).is_none(),
            "the satellite never sees the retained spawn"
        );
        assert!(older.pending_spawns.is_empty());
        assert!(matches!(
            rx.try_recv().expect("typed refusal"),
            SpawnResult::Err(SpawnError::SpawnFailed(message))
                if message.contains("lacks RETAIN_ON_EXIT")
        ));

        let (reply, _rx) = oneshot::channel();
        assert!(
            older.handle_request_checked(spawn_request(reply)).is_some(),
            "a spawn that omits retain_secs still relays"
        );
    }

    /// ADR-0126: a keyed spawn to a satellite without `SPAWN_IDEMPOTENCY`
    /// is refused; an unkeyed one relays.
    #[test]
    fn hub_refuses_a_keyed_satellite_spawn_when_the_satellite_lacks_the_bit() {
        let key = phux_protocol::ids::IdempotencyKey::new([7; 16]);
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, mut rx) = oneshot::channel();
        let mut request = spawn_request(reply);
        if let RelayRequest::Spawn { spawn, .. } = &mut request {
            spawn.resource = Some(Box::new(
                phux_protocol::wire::frame::SpawnResource::default().with_idempotency_key(key),
            ));
        }
        assert!(
            session.handle_request_checked(request).is_none(),
            "the satellite never sees the keyed spawn"
        );
        assert!(session.pending_spawns.is_empty());
        assert!(matches!(
            rx.try_recv().expect("typed refusal"),
            SpawnResult::Err(SpawnError::SpawnFailed(message))
                if message.contains("lacks SPAWN_IDEMPOTENCY")
        ));

        let (reply, _rx) = oneshot::channel();
        assert!(
            session
                .handle_request_checked(spawn_request(reply))
                .is_some(),
            "an unkeyed spawn to the same satellite still relays"
        );
    }

    /// ADR-0109: without `CONDITIONAL_KILL` the consumer is refused at once;
    /// with it the precondition crosses unchanged.
    #[test]
    fn a_conditional_kill_reaches_only_a_satellite_that_evaluates_it() {
        let command = Command::KillResourceIf {
            terminal_id: ResourceId::local(9),
            precondition: phux_protocol::wire::frame::KillPrecondition::spawned_and_unattached(
                phux_protocol::ids::ServerInstance::new([3; 16]),
            ),
            operation_id: None,
        };
        let mut older = RelaySession::new(host(), BootstrapLimits::default());
        let (reply, mut rx) = oneshot::channel();
        let wire = older.handle_request_checked(RelayRequest::Command {
            command: command.clone(),
            reply,
            subscribe: None,
        });
        assert!(wire.is_none(), "an older satellite never sees the tag");
        assert!(older.pending.is_empty());
        assert!(matches!(
            rx.try_recv().expect("typed refusal"),
            CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                ..
            }
        ));

        let mut current = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::with(&[ServerFeature::ConditionalKill]),
        );
        let (reply, _rx) = oneshot::channel();
        let wire = current
            .handle_request_checked(RelayRequest::Command {
                command: command.clone(),
                reply,
                subscribe: None,
            })
            .expect("relayed to a satellite that evaluates it");
        let FrameKind::Command {
            command: relayed, ..
        } = decode(&wire)
        else {
            panic!("expected COMMAND on the wire");
        };
        assert_eq!(relayed, command, "the precondition crosses unchanged");
    }

    // --- keyed operations and the incarnation fence (L1 §9.1) -----------

    fn keyed_session(
        features: &[ServerFeature],
        incarnation: [u8; 16],
        fence: &super::super::operation_fence::OperationFence,
    ) -> RelaySession {
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::with(features),
        );
        session.set_incarnation(Some(incarnation));
        session.set_operation_fence(fence.clone());
        session
    }

    fn apply_input_to(terminal_id: ResourceId) -> Command {
        Command::ApplyInput {
            operation_id: phux_protocol::InputOperationId::new([6; 16]).expect("id"),
            terminal_id,
            events: vec![],
        }
    }

    fn keyed_kill(byte: u8) -> Command {
        Command::KillResource {
            terminal_id: ResourceId::local(9),
            operation_id: phux_protocol::ids::IdempotencyKey::new([byte; 16]),
        }
    }

    /// Send `command` as `actor`'s keyed request: wire bytes and reply.
    fn send_keyed(
        session: &mut RelaySession,
        command: Command,
        actor: ClientId,
    ) -> (Option<Vec<u8>>, oneshot::Receiver<CommandResult>) {
        let (reply, rx) = oneshot::channel();
        let wire = session.handle_request_checked(RelayRequest::Keyed {
            command,
            actor,
            reply,
        });
        (wire, rx)
    }

    fn forwarded(wire: Option<Vec<u8>>) -> Command {
        let FrameKind::Command { command, .. } = decode(&wire.expect("forwarded to the satellite"))
        else {
            panic!("expected COMMAND on the wire");
        };
        command
    }

    #[test]
    fn hub_forwards_keyed_apply_input_to_a_satellite_that_advertises_the_bit() {
        let (target, command) =
            route_to_satellite(&apply_input_to(ResourceId::satellite(host(), 9)))
                .expect("APPLY_INPUT routes to its satellite");
        assert_eq!(target, host());
        assert_eq!(
            command,
            apply_input_to(ResourceId::local(9)),
            "the batch and its operation id cross verbatim"
        );
        let fence = super::super::operation_fence::OperationFence::default();
        let mut session = keyed_session(
            &[ServerFeature::AcknowledgedInput, ServerFeature::KeyedSignal],
            [1; 16],
            &fence,
        );
        let (wire, _rx) = send_keyed(&mut session, command.clone(), ClientId(4));
        assert_eq!(forwarded(wire), command);
        let (wire, _rx) = send_keyed(&mut session, keyed_kill(3), ClientId(4));
        assert_eq!(
            forwarded(wire),
            keyed_kill(3),
            "a keyed kill crosses with its key; the satellite dedupes it"
        );
    }

    #[test]
    fn hub_refuses_keyed_ops_to_a_satellite_without_the_bit_with_unsupported_satellite_route() {
        let fence = super::super::operation_fence::OperationFence::default();
        let mut older = keyed_session(&[], [1; 16], &fence);
        for command in [apply_input_to(ResourceId::local(9)), keyed_kill(3)] {
            let (wire, mut rx) = send_keyed(&mut older, command, ClientId(4));
            assert!(
                wire.is_none(),
                "an older satellite never sees the operation"
            );
            assert!(matches!(
                rx.try_recv().expect("typed refusal"),
                CommandResult::Error {
                    code: ErrorCode::UnsupportedSatelliteRoute,
                    ..
                }
            ));
        }
        assert!(older.pending.is_empty());
        let key = phux_protocol::ids::IdempotencyKey::new([3; 16]).expect("key");
        assert_eq!(
            fence.event_operation(&key, 9),
            super::super::operation_fence::EventOperation::Unknown,
            "a refused operation leaves nothing in the fence"
        );
        let unkeyed = Command::KillResource {
            terminal_id: ResourceId::local(9),
            operation_id: None,
        };
        let (reply, _rx) = oneshot::channel();
        assert!(
            older
                .handle_request_checked(RelayRequest::Command {
                    command: unkeyed,
                    reply,
                    subscribe: None,
                })
                .is_some(),
            "an unkeyed kill still relays to an older satellite"
        );
    }

    #[test]
    fn satellite_restart_between_attempts_yields_incarnation_changed_and_no_forward() {
        let fence = super::super::operation_fence::OperationFence::default();
        let features = [ServerFeature::AcknowledgedInput, ServerFeature::KeyedSignal];
        let mut before = keyed_session(&features, [1; 16], &fence);
        let (wire, _rx) = send_keyed(&mut before, keyed_kill(5), ClientId(4));
        assert!(wire.is_some(), "the first attempt reaches the satellite");
        let (wire, _rx) = send_keyed(
            &mut before,
            apply_input_to(ResourceId::local(9)),
            ClientId(4),
        );
        assert!(wire.is_some());

        let mut after = keyed_session(&features, [2; 16], &fence);
        for retry in [keyed_kill(5), apply_input_to(ResourceId::local(9))] {
            let (wire, mut rx) = send_keyed(&mut after, retry, ClientId(4));
            assert!(
                wire.is_none(),
                "a retry across the restart is not forwarded"
            );
            assert!(matches!(
                rx.try_recv().expect("typed refusal"),
                CommandResult::Error {
                    code: ErrorCode::IncarnationChanged,
                    ..
                }
            ));
        }
        assert!(after.pending.is_empty());
        let (wire, _rx) = send_keyed(&mut after, keyed_kill(6), ClientId(4));
        assert!(wire.is_some(), "a new id is new to the restarted satellite");
    }

    #[test]
    fn hub_restamps_a_satellite_pane_closed_with_the_consumers_actor_by_operation_id() {
        let journal = crate::state::SharedState::new();
        let consumer = journal.with_mut(crate::state::ServerState::new_client_id);
        let (registry_tx, mut registry_rx) = mpsc::channel(8);
        journal.with_mut(|s| {
            s.subscribe_satellite_events(
                consumer,
                ResourceId::satellite("devbox", 9),
                crate::state::EventFilter::all(),
                None,
                registry_tx,
            );
        });
        let fence = super::super::operation_fence::OperationFence::default();
        let mut session = keyed_session(&[ServerFeature::KeyedSignal], [1; 16], &fence);
        session.set_journal(Some(journal));
        let (proxy_tx, _proxy_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, consumer, proxy_tx);
        let (wire, _rx) = send_keyed(&mut session, keyed_kill(7), consumer);
        assert!(wire.is_some());

        let key = phux_protocol::ids::IdempotencyKey::new([7; 16]);
        let other = phux_protocol::ids::IdempotencyKey::new([8; 16]);
        for operation_id in [key, other] {
            let stamp = phux_protocol::wire::frame::EventStamp::new(40, 1_234)
                .with_operation_id(operation_id);
            session
                .handle_inbound(&encode(&FrameKind::Event {
                    terminal: Some(ResourceId::local(9)),
                    event: AgentEvent::ResourceClosed { exit_status: None },
                    stamp: Some(Box::new(stamp)),
                }))
                .expect("valid satellite frame");
        }
        let stamps: Vec<_> = std::iter::from_fn(|| registry_rx.try_recv().ok())
            .map(|out| match out {
                Outbound::Frame(FrameKind::Event { stamp, .. }) => {
                    stamp.expect("stamped by the hub")
                }
                other => panic!("expected an EVENT, got {other:?}"),
            })
            .collect();
        let [ours, theirs] = stamps.as_slice() else {
            panic!("two relayed pane_closed events: {stamps:?}");
        };
        assert_eq!(ours.operation_id, key, "the key crosses the hub");
        assert!(
            ours.actor.is_some(),
            "the event names the consumer that sent the keyed kill, not the link"
        );
        assert_eq!(theirs.operation_id, other);
        assert_eq!(
            theirs.actor, None,
            "an operation this hub never forwarded names no actor"
        );
    }

    /// L1 §9.1: every id inside a satellite's event is re-tagged or vetted
    /// before a hub consumer sees it. An operation id a consumer forwarded
    /// for one terminal cannot be claimed on another (neither the id nor
    /// the consumer survives); the satellite's client ids become the hub's
    /// lease holder or a foreign id; a resource parent is re-tagged; and an
    /// approval event naming a hub approval is dropped.
    #[allow(
        clippy::too_many_lines,
        reason = "one relayed-event scenario per id kind"
    )]
    #[test]
    fn satellite_event_ids_are_retagged_or_vetted_before_hub_consumers_see_them() {
        let journal = crate::state::SharedState::new();
        let consumer = journal.with_mut(crate::state::ServerState::new_client_id);
        let (registry_tx, mut registry_rx) = mpsc::channel(32);
        for terminal in [5, 9] {
            journal.with_mut(|s| {
                s.subscribe_satellite_events(
                    consumer,
                    ResourceId::satellite("devbox", terminal),
                    crate::state::EventFilter::all(),
                    None,
                    registry_tx.clone(),
                );
            });
        }
        let fence = super::super::operation_fence::OperationFence::default();
        let mut session = keyed_session(&[ServerFeature::KeyedSignal], [1; 16], &fence);
        session.set_journal(Some(journal.clone()));
        let (proxy_tx, _proxy_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, consumer, proxy_tx.clone());
        subscribe(&mut session, 5, consumer, proxy_tx);
        let (wire, _rx) = send_keyed(&mut session, keyed_kill(7), consumer);
        assert!(wire.is_some(), "the kill for terminal 9 is forwarded");
        let mut relay = |terminal: u32, event: AgentEvent, stamp: Option<Box<_>>| {
            session
                .handle_inbound(&encode(&FrameKind::Event {
                    terminal: Some(ResourceId::local(terminal)),
                    event,
                    stamp,
                }))
                .expect("valid satellite frame");
            std::iter::from_fn(|| registry_rx.try_recv().ok())
                .filter_map(|out| match out {
                    Outbound::Frame(FrameKind::Event { event, stamp, .. }) => Some((event, stamp)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let control = |holder: u32| AgentEvent::TerminalControl {
            lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
            exit_status: None,
            input_holder: Some(phux_protocol::ids::ClientId::new(holder)),
            action: ControlAction::Interrupted,
            actor: Some(phux_protocol::ids::ClientId::new(holder)),
        };
        let wire_consumer = crate::runtime::wire_client(consumer);

        // The kill's key, claimed on terminal 5: dropped with its consumer.
        let key = phux_protocol::ids::IdempotencyKey::new([7; 16]);
        let stamp = phux_protocol::wire::frame::EventStamp::new(1, 1).with_operation_id(key);
        let got = relay(5, control(wire_consumer.get()), Some(Box::new(stamp)));
        let [(event, Some(stamp))] = got.as_slice() else {
            panic!("one relayed event: {got:?}");
        };
        assert_eq!(
            stamp.operation_id, None,
            "the id is not the satellite's to claim"
        );
        assert_eq!(stamp.actor, None);
        let AgentEvent::TerminalControl {
            input_holder,
            actor,
            ..
        } = event
        else {
            panic!("expected terminal_control: {event:?}");
        };
        assert_ne!(
            *input_holder,
            Some(wire_consumer),
            "a satellite client id never reads as a hub consumer"
        );
        assert_eq!(*input_holder, Some(foreign_client(wire_consumer)));
        assert_eq!(*actor, Some(foreign_client(wire_consumer)));

        // On terminal 9 the same key is the consumer's own operation, and
        // the hub's lease ledger names the holder.
        let (lease_tx, _lease_rx) = mpsc::channel(1);
        journal.with_mut(|s| {
            s.set_satellite_lease(host(), 9, consumer, lease_tx);
        });
        let stamp = phux_protocol::wire::frame::EventStamp::new(2, 1).with_operation_id(key);
        let got = relay(9, control(1), Some(Box::new(stamp)));
        let [(event, Some(stamp))] = got.as_slice() else {
            panic!("one relayed event: {got:?}");
        };
        assert_eq!(stamp.operation_id, key);
        assert!(stamp.actor.is_some(), "the consumer's own kill names it");
        let AgentEvent::TerminalControl {
            input_holder,
            actor,
            ..
        } = event
        else {
            panic!("expected terminal_control: {event:?}");
        };
        assert_eq!(
            *input_holder,
            Some(wire_consumer),
            "the hub's ledger holder"
        );
        assert_eq!(*actor, Some(wire_consumer), "the operation's sender");

        // A child's parent is re-tagged into the hub's id space.
        let got = relay(
            5,
            AgentEvent::ResourceSpawned {
                kind: phux_protocol::ids::ResourceKind::AgentSession,
                parent: Some(ResourceId::local(9)),
            },
            None,
        );
        assert!(
            matches!(
                got.as_slice(),
                [(AgentEvent::ResourceSpawned { parent: Some(parent), .. }, _)]
                    if *parent == ResourceId::satellite(host(), 9)
            ),
            "{got:?}"
        );

        // An approval event naming the hub's own pending approval is dropped.
        let held = Command::KillResource {
            terminal_id: ResourceId::satellite(host(), 9),
            operation_id: None,
        };
        let approval = journal
            .with_mut(|s| s.open_approval(consumer, &held))
            .expect("hold opened")
            .id;
        let got = relay(
            9,
            AgentEvent::ApprovalDecided {
                id: approval,
                outcome: phux_protocol::wire::frame::ApprovalOutcome::Denied,
            },
            None,
        );
        assert!(
            !got.iter()
                .any(|(event, _)| matches!(event, AgentEvent::ApprovalDecided { .. })),
            "{got:?}"
        );
        assert!(
            journal.with(|s| s.pending_approval(approval).is_some()),
            "the hub's approval is untouched"
        );
    }

    #[test]
    fn instance_only_close_preserves_satellite_fence_and_correlates_refusal() {
        use phux_protocol::wire::frame::{KillConditions, KillPrecondition};
        let precondition = KillPrecondition {
            instance: Some(phux_protocol::ids::ServerInstance::new([3; 16])),
            conditions: KillConditions::NONE,
        };
        let (target, command) = route_kill_to_satellite(&Command::KillResourceIf {
            terminal_id: ResourceId::satellite(host(), 9),
            precondition,
            operation_id: None,
        })
        .expect("satellite route");
        assert_eq!(target, host());
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::with(&[ServerFeature::ConditionalKill]),
        );
        let (reply, mut rx) = oneshot::channel();
        let wire = session
            .handle_request_checked(RelayRequest::Command {
                command,
                reply,
                subscribe: None,
            })
            .expect("instance-only kill reaches capable satellite");
        let FrameKind::Command {
            request_id,
            command,
        } = decode(&wire)
        else {
            panic!("expected command");
        };
        assert_eq!(
            command,
            Command::KillResourceIf {
                terminal_id: ResourceId::local(9),
                precondition,
                operation_id: None,
            }
        );
        let refusal = CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: "satellite incarnation changed".into(),
        };
        session
            .handle_inbound(&encode(&FrameKind::CommandResult {
                request_id,
                result: refusal.clone(),
            }))
            .expect("valid satellite refusal");
        assert_eq!(rx.try_recv().expect("correlated outcome"), refusal);
        assert!(session.pending.is_empty());
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
        let expected = FrameKind::SpawnResource {
            request_id: 0,
            group: spawn.group,
            command: spawn.command.clone(),
            cwd: spawn.cwd.clone(),
            env: spawn.env.clone(),
            term: spawn.term.clone(),
            satellite: None,
            owner_terminal: Some(ResourceId::local(91)),
            agent_session: None,
            initial_size: Some((132, 43)),
            resource: None,
        };
        let consumer = handle.spawn(spawn);
        let satellite = async {
            let request = mailbox.requests.recv().await.expect("spawn enqueued");
            let mut session = RelaySession::new(host(), BootstrapLimits::default());
            let mut frame = decode(&session.handle_request(request));
            let FrameKind::SpawnResource { request_id, .. } = &mut frame else {
                panic!("spawn frame");
            };
            let link_request_id = *request_id;
            *request_id = 0;
            assert_eq!(frame, expected);
            session
                .handle_inbound(&encode(&FrameKind::ResourceSpawned {
                    request_id: link_request_id,
                    result: SpawnResult::Ok(ResourceId::local(92)),
                }))
                .expect("spawn reply");
        };
        let (result, ()) = tokio::join!(consumer, satellite);
        assert_eq!(result, SpawnResult::Ok(ResourceId::satellite("devbox", 92)));
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

    // --- session: journal re-stamp (ADR-0123) -----------------------------

    #[test]
    fn relayed_events_go_through_the_hub_registry_with_the_hubs_seq() {
        let journal = crate::state::SharedState::new();
        let consumer = journal.with_mut(crate::state::ServerState::new_client_id);
        let (registry_tx, mut registry_rx) = mpsc::channel(8);
        journal.with_mut(|s| {
            s.subscribe_satellite_events(
                consumer,
                ResourceId::satellite("devbox", 9),
                crate::state::EventFilter::all(),
                None,
                registry_tx,
            );
        });
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        session.set_journal(Some(journal.clone()));
        let (proxy_tx, mut proxy_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, consumer, proxy_tx);
        let actor = phux_protocol::wire::frame::ActorRef::new(phux_protocol::ids::ClientId::new(4))
            .with_client_name(Some("satellite-side".to_owned()));
        let satellite_stamp =
            phux_protocol::wire::frame::EventStamp::new(77, 1_234).with_actor(Some(actor));
        let scoped_gap = AgentEvent::JournalGap {
            first_missing: 5,
            last_missing: 7,
        };
        let link_gap = AgentEvent::JournalGap {
            first_missing: 1,
            last_missing: 4,
        };
        for (terminal, event, stamp) in [
            (
                Some(ResourceId::local(9)),
                AgentEvent::CommandStarted,
                Some(Box::new(satellite_stamp)),
            ),
            (Some(ResourceId::local(9)), scoped_gap, None),
            (None, link_gap, None),
        ] {
            session
                .handle_inbound(&encode(&FrameKind::Event {
                    terminal,
                    event,
                    stamp,
                }))
                .expect("valid satellite frame");
        }
        let delivered: Vec<_> = std::iter::from_fn(|| registry_rx.try_recv().ok())
            .map(|out| match out {
                Outbound::Frame(FrameKind::Event {
                    terminal,
                    event,
                    stamp,
                }) => (terminal, event, stamp.expect("stamped by the hub")),
                other => panic!("expected an EVENT, got {other:?}"),
            })
            .collect();
        let [(t1, started, first), (t2, gap, second), (t3, link, third)] = delivered.as_slice()
        else {
            panic!("three events through the registry: {delivered:?}");
        };
        let scope = Some(ResourceId::satellite("devbox", 9));
        assert!([t1, t2, t3].iter().all(|t| **t == scope), "re-tagged");
        assert_eq!(*started, AgentEvent::CommandStarted);
        assert_eq!(first.seq, 1, "the hub's seq, not the satellite's 77");
        assert_eq!(first.ts_ms, 1_234, "the satellite's time crosses the hub");
        assert_eq!(first.actor, None, "the satellite's actor does not");
        assert_eq!(*gap, AgentEvent::SourceGap { dropped: 3 });
        assert_eq!(*link, AgentEvent::SourceGap { dropped: 4 });
        assert_eq!((second.seq, third.seq), (2, 3));
        assert!(
            proxy_rx.try_recv().is_err(),
            "events are delivered once, by the registry, not by the relay"
        );
        assert_eq!(journal.with(crate::state::ServerState::journal_head), 3);
    }

    // --- lease TTL mirror (ADR-0033) ---

    /// A satellite `EXPIRED`/`RELEASED` frees the hub's ledger entry.
    #[test]
    fn a_satellite_expiry_or_release_clears_the_hubs_satellite_lease() {
        for action in [ControlAction::Expired, ControlAction::Released] {
            let journal = crate::state::SharedState::new();
            let holder = journal.with_mut(crate::state::ServerState::new_client_id);
            let (mailbox_tx, _mailbox_rx) = mpsc::channel(1);
            journal.with_mut(|s| {
                s.set_satellite_lease(host(), 9, holder, mailbox_tx.clone());
            });
            assert_eq!(
                journal.with(|s| s.satellite_lease_holder(&host(), 9)),
                Some(holder),
                "{action:?}: precondition"
            );
            let mut session = RelaySession::new(host(), BootstrapLimits::default());
            session.set_journal(Some(journal.clone()));
            session
                .handle_inbound(&encode(&FrameKind::Event {
                    terminal: Some(ResourceId::local(9)),
                    event: AgentEvent::TerminalControl {
                        lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
                        exit_status: None,
                        input_holder: None,
                        action,
                        actor: None,
                    },
                    stamp: None,
                }))
                .expect("valid satellite frame");
            assert_eq!(
                journal.with(|s| s.satellite_lease_holder(&host(), 9)),
                None,
                "{action:?}: the hub ledger must follow the satellite"
            );
        }
    }

    /// A `SEIZED` is not a free lease; the ledger is left alone.
    #[test]
    fn a_satellite_seize_does_not_clear_the_hubs_satellite_lease() {
        let journal = crate::state::SharedState::new();
        let holder = journal.with_mut(crate::state::ServerState::new_client_id);
        let (mailbox_tx, _mailbox_rx) = mpsc::channel(1);
        journal.with_mut(|s| {
            s.set_satellite_lease(host(), 9, holder, mailbox_tx);
        });
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        session.set_journal(Some(journal.clone()));
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::TerminalControl {
                    lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
                    exit_status: None,
                    input_holder: Some(phux_protocol::ids::ClientId::new(99)),
                    action: ControlAction::Seized,
                    actor: None,
                },
                stamp: None,
            }))
            .expect("valid satellite frame");
        assert_eq!(
            journal.with(|s| s.satellite_lease_holder(&host(), 9)),
            Some(holder),
            "a SEIZED mirror must not evict the hub ledger"
        );
    }

    /// A stale Released that arrives after a newer acquire's reply must not
    /// evict the new holder.
    #[test]
    fn a_stale_released_arriving_after_a_newer_acquires_reply_does_not_evict_it() {
        let journal = crate::state::SharedState::new();
        let a = journal.with_mut(crate::state::ServerState::new_client_id);
        let b = journal.with_mut(crate::state::ServerState::new_client_id);
        let (a_tx, _a_rx) = mpsc::channel(1);
        let (b_tx, _b_rx) = mpsc::channel(1);

        // A held the lease.
        journal.with_mut(|s| {
            s.set_satellite_lease(host(), 9, a, a_tx);
        });

        // B's acquire relay marks pending...
        journal.with_mut(|s| s.mark_satellite_lease_acquire_pending(host(), 9));
        // ...and its reply lands first; only the event mirror clears the mark.
        journal.with_mut(|s| {
            s.set_satellite_lease(host(), 9, b, b_tx);
        });
        assert_eq!(
            journal.with(|s| s.satellite_lease_holder(&host(), 9)),
            Some(b),
            "precondition: the reply already installed B"
        );

        // A's delayed Released arrives.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        session.set_journal(Some(journal.clone()));
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::TerminalControl {
                    lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
                    exit_status: None,
                    input_holder: None,
                    action: ControlAction::Released,
                    actor: None,
                },
                stamp: None,
            }))
            .expect("valid satellite frame");

        assert_eq!(
            journal.with(|s| s.satellite_lease_holder(&host(), 9)),
            Some(b),
            "a stale Released arriving after a newer reply must not evict the holder it installed"
        );

        // The mark is consumed; a later Released applies normally.
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::TerminalControl {
                    lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
                    exit_status: None,
                    input_holder: None,
                    action: ControlAction::Released,
                    actor: None,
                },
                stamp: None,
            }))
            .expect("valid satellite frame");
        assert_eq!(
            journal.with(|s| s.satellite_lease_holder(&host(), 9)),
            None,
            "a later, non-racing Released is honored normally"
        );
    }

    /// L1 §7.1 / ADR-0123: journal semantics on the link only when the
    /// satellite speaks them.
    #[test]
    fn the_link_subscribes_events_with_journal_semantics_when_the_satellite_speaks_them() {
        for (features, cursor) in [
            (
                ServerFeatureSet::with(&[ServerFeature::EventJournal]),
                Some(u64::MAX),
            ),
            (ServerFeatureSet::new(), None),
        ] {
            let mut session = RelaySession::new_negotiated(
                host(),
                BootstrapLimits::default(),
                BootstrapProfile::SynthesizedVtRaw,
                features,
            );
            let (out_tx, _out_rx) = mpsc::channel(1);
            let wire = session.handle_request(RelayRequest::Subscribe {
                subscription: ProxySubscription {
                    terminal: 9,
                    client: ClientId(1),
                    out_tx,
                    consumer_cancel: CancellationToken::new(),
                    seq: 1,
                    awaits_snapshot: false,
                    bootstrap_profile: None,
                    bootstrap_limits: None,
                },
                forward: FrameKind::SubscribeEvents {
                    terminal: Some(ResourceId::local(9)),
                    after_seq: None,
                },
            });
            assert!(
                matches!(
                    decode(&wire),
                    FrameKind::SubscribeEvents { after_seq, .. } if after_seq == cursor
                ),
                "forwarded cursor must be {cursor:?}"
            );
        }
    }

    // --- session: return-leg re-tagging ----------------------------------

    #[test]
    fn session_retags_subscribed_streams_local_to_satellite() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);

        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }))
            .expect("valid satellite frame");
        session
            .handle_inbound(&encode(&FrameKind::ResourceOutput {
                terminal_id: ResourceId::local(9),
                stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
                seq: 42,
                bytes: bytes::Bytes::from_static(b"hi"),
            }))
            .expect("valid satellite frame");
        // A different terminal: nothing must reach the subscriber.
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(10)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }))
            .expect("valid satellite frame");

        let Outbound::Frame(first) = out_rx.try_recv().expect("event fanned out") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            first,
            FrameKind::Event {
                terminal: Some(ResourceId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }
        );
        let Outbound::Frame(second) = out_rx.try_recv().expect("output fanned out") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(matches!(
            second,
            FrameKind::ResourceOutput { terminal_id, seq: 42, .. }
                if terminal_id == ResourceId::satellite("devbox", 9)
        ));
        assert!(out_rx.try_recv().is_err(), "unsubscribed terminal leaked");
    }

    #[test]
    fn session_drops_chained_satellite_tags_from_the_return_leg() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::satellite("nested", 9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
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
            .handle_inbound(&encode(&FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("closed fanned out") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::satellite("devbox", 9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            }
        );
        // Subscription is gone: further frames for id 9 do not fan out.
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }))
            .expect("valid satellite frame");
        assert!(out_rx.try_recv().is_err());
    }

    #[test]
    fn terminal_close_follows_retained_output_before_subscription_removal() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(1);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        out_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill downstream mailbox");
        session.fan_out(9, &output_frame(9, 2, b"before-close"));
        session
            .handle_inbound(&encode(&FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            }))
            .expect("valid close");
        assert!(session.subscribers.contains_key(&9));

        let _ = out_rx.try_recv().expect("filler");
        session.flush_pending_snapshots();
        assert!(matches!(
            out_rx.try_recv().expect("retained output first"),
            Outbound::Frame(FrameKind::ResourceOutput { .. })
        ));
        session.flush_pending_snapshots();
        assert!(matches!(
            out_rx.try_recv().expect("close follows output"),
            Outbound::Frame(FrameKind::ResourceClosed { .. })
        ));
        assert!(!session.subscribers.contains_key(&9));
        assert!(
            session.take_delivery_detaches().is_empty(),
            "a resource already closed upstream needs no detach after downstream delivery"
        );
    }

    #[test]
    fn idempotent_reattach_cannot_cancel_a_queued_terminal_close() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (old_tx, _old_rx) = mpsc::channel(1);
        old_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill old mailbox");
        subscribe(&mut session, 9, ClientId(1), old_tx);
        session.fan_out(9, &output_frame(9, 2, b"before-close"));
        session
            .handle_inbound(&encode(&FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            }))
            .expect("valid close");

        let (new_tx, mut new_rx) = mpsc::channel(2);
        let registration = session.register_subscriber(ProxySubscription {
            terminal: 9,
            client: ClientId(1),
            out_tx: new_tx,
            consumer_cancel: CancellationToken::new(),
            seq: 0,
            awaits_snapshot: true,
            bootstrap_profile: None,
            bootstrap_limits: None,
        });
        assert!(matches!(registration, Registration::Idempotent));
        assert!(session.subscribers[&9][0].retire_after_flush);

        session.flush_pending_snapshots();
        assert!(matches!(
            new_rx.try_recv().expect("retained output"),
            Outbound::Frame(FrameKind::ResourceOutput { .. })
        ));
        assert!(matches!(
            new_rx.try_recv().expect("queued close"),
            Outbound::Frame(FrameKind::ResourceClosed { .. })
        ));
        assert!(!session.subscribers.contains_key(&9));
        assert!(session.take_delivery_detaches().is_empty());
    }

    #[test]
    fn close_overflow_fences_the_terminal_generation_without_detaching_closed_upstream() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, _out_rx) = mpsc::channel(1);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        let stream_id = StreamId::new(4).expect("stream");
        let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
        let sub = &mut session.subscribers.get_mut(&9).expect("subscriber")[0];
        sub.current_generation = Some((stream_id, bootstrap_id, 17));
        sub.gate = SnapshotGate::Retained {
            frames: VecDeque::from([output_frame(9, 17, b"retained")]),
            retained_bytes: MAX_RELAY_SUBSCRIBER_RETAINED_BYTES,
            open_after: true,
        };
        session.retained_bytes = MAX_RELAY_SUBSCRIBER_RETAINED_BYTES;
        session.retained_frames = 1;

        session
            .handle_inbound(&encode(&FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            }))
            .expect("valid close");
        let SnapshotGate::Retiring {
            failure,
            detach_upstream,
            ..
        } = &session.subscribers[&9][0].gate
        else {
            panic!("close overflow retires with an explicit generation fence")
        };
        assert!(!detach_upstream);
        assert!(matches!(
            failure,
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id: failed_stream,
                bootstrap_id: failed_bootstrap,
                last_valid_seq: 17,
                reason: TombstoneReason::OutboundGap,
            } if terminal_id == &ResourceId::satellite("devbox", 9)
                && *failed_stream == stream_id
                && *failed_bootstrap == bootstrap_id
        ));
        assert!(session.take_delivery_detaches().is_empty());
    }

    #[test]
    fn terminal_close_supersedes_an_existing_delivery_gap_retirement() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(1);
        out_tx
            .try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill downstream mailbox");
        subscribe(&mut session, 9, ClientId(1), out_tx);
        for chunk_seq in 0..=MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES {
            session.fan_out(
                9,
                &FrameKind::BootstrapChunk {
                    terminal_id: ResourceId::satellite("devbox", 9),
                    stream_id: StreamId::new(1).expect("stream"),
                    bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                    chunk_seq: u32::try_from(chunk_seq).expect("small sequence"),
                    payload: bytes::Bytes::from_static(b"x"),
                },
            );
        }
        assert!(matches!(
            session.subscribers[&9][0].gate,
            SnapshotGate::Retiring {
                failure: FrameKind::BootstrapTombstone { .. },
                detach_upstream: true,
                ..
            }
        ));

        session
            .handle_inbound(&encode(&FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(9),
                exit_status: Some(23),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            }))
            .expect("valid close");
        assert!(matches!(
            session.subscribers[&9][0].gate,
            SnapshotGate::Retiring {
                failure: FrameKind::ResourceClosed {
                    exit_status: Some(23),
                    ..
                },
                detach_upstream: false,
                ..
            }
        ));

        let _ = out_rx.try_recv().expect("filler");
        session.flush_pending_snapshots();
        assert!(matches!(
            out_rx.try_recv().expect("authoritative close"),
            Outbound::Frame(FrameKind::ResourceClosed {
                exit_status: Some(23),
                ..
            })
        ));
        assert!(!session.subscribers.contains_key(&9));
        assert!(session.take_delivery_detaches().is_empty());
    }

    // --- attach snapshot ordering under backpressure (phux-v45.12) --------

    fn snapshot_frame(id: u32) -> FrameKind {
        FrameKind::BootstrapReady {
            terminal_id: ResourceId::local(id),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            history_cursor: None,
        }
    }

    fn begin_frame(id: u32, generation: u64) -> FrameKind {
        FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(id),
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
        FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(id),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            seq,
            bytes: bytes::Bytes::from_static(bytes),
        }
    }

    #[tokio::test]
    async fn full_mailbox_retains_the_snapshot_so_a_delta_never_overtakes_it() {
        // A snapshot refused by a full mailbox is retained, so a later delta
        // cannot reach the consumer first (L1 §9.1).
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
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
                FrameKind::ResourceOutput { ref terminal_id, seq: 1, .. }
                    if *terminal_id == ResourceId::satellite("devbox", 9)
            ),
            "the delta must ride after the snapshot, re-tagged, got {second:?}"
        );
    }

    #[tokio::test]
    async fn deltas_are_suppressed_until_the_retained_snapshot_flushes_on_the_tick() {
        // A stuck snapshot suppresses deltas; the keepalive flush converges.
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

        assert!(matches!(
            out_rx.try_recv().expect("filler drains"),
            Outbound::Frame(FrameKind::Detach)
        ));
        assert!(
            out_rx.try_recv().is_err(),
            "no frame may reach the consumer while the snapshot is stuck"
        );

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
        // A newer snapshot replaces a retained one.
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
        // B attaches while A streams: B must not see A's delta before its own
        // snapshot (L1 §9.1).
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

        attach(&mut session, 9, ClientId(2), tx_b);

        session
            .handle_inbound(&encode(&output_frame(9, 1, b"a-stream")))
            .expect("valid satellite frame");
        let Outbound::Frame(a_delta) = rx_a.try_recv().expect("A sees the delta") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert!(matches!(a_delta, FrameKind::ResourceOutput { seq: 1, .. }));
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
                FrameKind::ResourceOutput { ref terminal_id, seq: 2, .. }
                    if *terminal_id == ResourceId::satellite("devbox", 9)
            ),
            "B's delta must follow its snapshot, re-tagged, got {b_delta:?}"
        );
    }

    #[test]
    fn a_gated_attach_still_receives_terminal_closed_before_being_reaped() {
        // A gated subscriber still receives RESOURCE_CLOSED.
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
            .handle_inbound(&encode(&FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = rx_b.try_recv().expect("close delivered past the gate") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::satellite("devbox", 9),
                exit_status: Some(0),
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            },
            "a gated subscriber must still see RESOURCE_CLOSED"
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
        // Event-only subscriptions carry no snapshot and flow at once.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("event flows without a snapshot")
        else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::Event {
                terminal: Some(ResourceId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }
        );
    }

    /// L1 §7.3: the hub drops the satellite's journal stamp.
    #[test]
    fn a_satellite_event_stamp_does_not_cross_the_hub() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx);
        let stamp =
            phux_protocol::wire::frame::EventStamp::new(41, 1_700_000_000_000).with_actor(Some(
                phux_protocol::wire::frame::ActorRef::new(phux_protocol::ids::ClientId::new(3)),
            ));
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: Some(Box::new(stamp)),
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = out_rx.try_recv().expect("the event flows") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::Event {
                terminal: Some(ResourceId::satellite("devbox", 9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }
        );
    }

    #[test]
    fn subscribe_then_attach_upgrade_gates_deltas_until_the_attach_snapshot() {
        // An event subscriber upgrading to an attach is re-gated until the
        // attach's snapshot (L1 §9.1).
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);

        // 1. Event-only subscribe: gate Open, an EVENT flows immediately.
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }))
            .expect("valid satellite frame");
        assert!(
            out_rx.try_recv().is_ok(),
            "the event-only stream must be flowing before the upgrade"
        );

        // 2. UPGRADE the same client to an attach on the same terminal.
        attach(&mut session, 9, ClientId(1), out_tx);

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
                FrameKind::ResourceOutput { ref terminal_id, seq: 2, .. }
                    if *terminal_id == ResourceId::satellite("devbox", 9)
            ),
            "the delta must follow the snapshot, re-tagged, got {delta:?}"
        );
    }

    #[test]
    fn an_event_only_re_subscribe_does_not_re_gate_a_flowing_stream() {
        // An event-only re-subscribe leaves an open stream flowing.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        // A second event-only subscribe for the same client (idempotent).
        subscribe(&mut session, 9, ClientId(1), out_tx);
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }))
            .expect("valid satellite frame");
        assert!(
            out_rx.try_recv().is_ok(),
            "an event-only re-subscribe must not re-gate the flowing stream"
        );
    }

    #[test]
    fn a_gated_attach_still_receives_a_bell_past_the_gate() {
        // A bell passes an AwaitingFirst gate.
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
                terminal_id: ResourceId::local(9),
            }))
            .expect("valid satellite frame");
        let Outbound::Frame(frame) = rx.try_recv().expect("bell delivered past the gate") else {
            panic!("unexpected terminal outbound sentinel")
        };
        assert_eq!(
            frame,
            FrameKind::Bell {
                terminal_id: ResourceId::satellite("devbox", 9),
            },
            "a gated subscriber must still see a BELL"
        );

        // ...without opening it.
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
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }))
            .expect("valid satellite frame");
        assert!(out_rx.try_recv().is_err());
        // The last proxy left: detach upstream.
        assert_eq!(frames.len(), 1, "one satellite-side detach expected");
        let FrameKind::Command { command, .. } = decode(&frames[0]) else {
            panic!("expected COMMAND on the wire");
        };
        assert_eq!(
            command,
            Command::DetachResource {
                terminal_id: ResourceId::local(9),
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
        // Client 2 still observes it: no upstream detach.
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
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
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
        // Reorder guard: a stale detach (token 2) applied after a re-attach
        // (token 3) that raced ahead is dropped.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
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
            .handle_inbound(&encode(&FrameKind::ResourceOutput {
                terminal_id: ResourceId::local(9),
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
            FrameKind::ResourceOutput { terminal_id, seq: 7, .. }
                if terminal_id == ResourceId::satellite("devbox", 9)
        ));

        // A newer detach still withdraws.
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
        // Unsubscribes ride an unbounded channel.
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
        // No session: dropping the receipt certifies no proxy can emit.
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
            Ok(Outbound::Frame(FrameKind::ResourceOutput { .. }))
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
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        session.handle_inbound(&encode(&begin_frame(9, 1))).unwrap();
        session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
        let (events_tx, mut events_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(2), events_tx);
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let (reply, _received) = oneshot::channel();
        let request = RelayRequest::Command {
            command: Command::AttachResource {
                terminal_id: ResourceId::local(9),
                role_policy: None,
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 9,
                client: ClientId(1),
                out_tx,
                consumer_cancel: CancellationToken::new(),
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
                terminal: Some(ResourceId::Local { id: 9 }),
                ..
            }
        ));
        let FrameKind::Command {
            request_id,
            command: Command::DetachResource { terminal_id },
        } = decode(barrier)
        else {
            panic!("detach barrier");
        };
        assert_eq!(terminal_id, ResourceId::local(9));
        assert!(
            session.prepare_request(&request).is_empty(),
            "reuse an in-flight barrier"
        );
        session.handle_request_checked(request).unwrap();
        let event = FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: phux_protocol::wire::frame::AgentEvent::CommandStarted,
            stamp: None,
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
        // A prefix queued before withdrawal opens no new gate.
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
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
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
        // Register + forward is atomic: on a full mailbox nothing registers
        // and the consumer gets an error push.
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
                consumer_cancel: CancellationToken::new(),
                // Stamped by `handle.subscribe` at enqueue.
                seq: 0,
                awaits_snapshot: false,
                bootstrap_profile: None,
                bootstrap_limits: None,
            },
            FrameKind::SubscribeEvents {
                terminal: Some(ResourceId::local(9)),
                after_seq: None,
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
        // A refused subscribing command leaves no registration.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let (reply, mut reply_rx) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::AttachResource {
                terminal_id: ResourceId::local(9),
                role_policy: None,
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 9,
                client: ClientId(1),
                out_tx,
                consumer_cancel: CancellationToken::new(),
                seq: 1,
                // Ungated so a rollback regression would visibly leak.
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
            .handle_inbound(&encode(&FrameKind::ResourceOutput {
                terminal_id: ResourceId::local(9),
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
        // B upgrades (re-gated), C attaches and B retains C's snapshot, then
        // B's upgrade errors: the rollback must not replace Retained with
        // Open (L1 §9.1).
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (tx_b, mut rx_b) = mpsc::channel(2);
        let (tx_c, mut rx_c) = mpsc::channel(8);

        // B's event-only stream is established and demonstrably Open.
        subscribe(&mut session, 9, ClientId(2), tx_b.clone());
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::CommandStarted,
                stamp: None,
            }))
            .expect("valid satellite frame");
        assert!(rx_b.try_recv().is_ok(), "B's event stream must be open");

        let (reply_b, mut reply_rx_b) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::AttachResource {
                terminal_id: ResourceId::local(9),
                role_policy: None,
            },
            reply: reply_b,
            subscribe: Some(ProxySubscription {
                terminal: 9,
                client: ClientId(2),
                out_tx: tx_b.clone(),
                consumer_cancel: CancellationToken::new(),
                seq: 2,
                awaits_snapshot: true,
                bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: Some(BootstrapLimits::default()),
            }),
        });
        let FrameKind::Command { request_id, .. } = decode(&wire) else {
            panic!("expected B's attach COMMAND");
        };

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

        // B's next delta flushes the retained snapshot first.
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
            Outbound::Frame(FrameKind::ResourceOutput { seq: 1, .. })
        ));
    }

    #[test]
    fn satellite_error_never_rolls_back_a_preexisting_subscription() {
        // An errored upgrade re-subscribe restores the original stream's gate.
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = mpsc::channel(8);
        subscribe(&mut session, 9, ClientId(1), out_tx.clone());
        let (reply, _reply_rx) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::AttachResource {
                terminal_id: ResourceId::local(9),
                role_policy: None,
            },
            reply,
            subscribe: Some(ProxySubscription {
                terminal: 9,
                client: ClientId(1),
                out_tx,
                consumer_cancel: CancellationToken::new(),
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
            .handle_inbound(&encode(&FrameKind::ResourceOutput {
                terminal_id: ResourceId::local(9),
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
        // The mailbox accepts, nothing answers; paused time hits the deadline.
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

        // A late reply for the pruned id is dropped.
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

    // --- relayed LIST_DIRECTORY (docs/spec/L3.md §4.1) ---------------------

    fn listing_request(path: &str) -> (RelayRequest, oneshot::Receiver<DirectoryListingResult>) {
        let (reply, rx) = oneshot::channel();
        (
            RelayRequest::ListDirectory {
                path: path.to_owned(),
                reply,
            },
            rx,
        )
    }

    fn refusal_message(result: DirectoryListingResult) -> String {
        let refusal = result.expect_err("expected a refusal");
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        refusal.message
    }

    #[test]
    fn session_relays_a_listing_under_its_own_id_and_resolves_the_reply() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        // Occupy id 1 so the listing proves it shares the allocator.
        let _ = session.handle_request(RelayRequest::Command {
            command: Command::Upgrade,
            reply: oneshot::channel().0,
            subscribe: None,
        });
        let (request, mut rx) = listing_request("/srv");
        let wire = session.handle_request(request);
        let FrameKind::ListDirectory {
            request_id,
            path,
            host: forwarded_host,
        } = decode(&wire)
        else {
            panic!("expected LIST_DIRECTORY on the link");
        };
        assert_eq!(
            request_id, 2,
            "link-side id comes from the shared allocator"
        );
        assert_eq!(path, "/srv");
        assert_eq!(forwarded_host, None, "the satellite lists its own host");

        let listing = phux_protocol::wire::frame::DirectoryListing {
            path: "/srv".to_owned(),
            parent: Some("/".to_owned()),
            entries: Vec::new(),
            truncated: false,
        };
        session
            .handle_inbound(&encode(&FrameKind::DirectoryListing {
                request_id,
                result: Ok(listing.clone()),
            }))
            .expect("valid satellite frame");
        assert_eq!(rx.try_recv().expect("resolved"), Ok(listing));
    }

    #[test]
    fn a_satellite_without_list_directory_is_refused_before_the_link() {
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::new(),
        );
        let (request, mut rx) = listing_request("/srv");
        assert!(session.handle_request_checked(request).is_none());
        let message = refusal_message(rx.try_recv().expect("resolved at once"));
        assert!(
            message.contains("devbox") && message.contains("LIST_DIRECTORY"),
            "{message}"
        );
    }

    #[test]
    fn a_correlated_error_and_teardown_both_refuse_pending_listings() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (first, mut first_rx) = listing_request("/a");
        let FrameKind::ListDirectory { request_id, .. } = decode(&session.handle_request(first))
        else {
            panic!("expected LIST_DIRECTORY");
        };
        session
            .handle_inbound(&encode(&FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::InvalidCommand,
                message: "nope".to_owned(),
            }))
            .expect("valid satellite frame");
        let message = refusal_message(first_rx.try_recv().expect("resolved"));
        assert!(message.contains("refused the listing"), "{message}");

        let (second, mut second_rx) = listing_request("/b");
        let _ = session.handle_request(second);
        session.teardown("link lost");
        let refusal = second_rx
            .try_recv()
            .expect("resolved")
            .expect_err("refused");
        assert_eq!(refusal.path, "/b");
        assert!(
            refusal.message.contains("unreachable: link lost"),
            "{}",
            refusal.message
        );
    }

    #[tokio::test]
    async fn fail_fast_refuses_a_listing_naming_the_host() {
        let (request, rx) = listing_request("/srv");
        fail_fast(request, &host(), "backoff");
        let message = refusal_message(rx.await.expect("resolved"));
        assert!(
            message.contains("devbox is unreachable: backoff"),
            "{message}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_listing_times_out_against_a_silent_satellite() {
        let (handle, rx) = RelayHandle::new(host());
        let message = refusal_message(handle.list_directory("/srv".to_owned()).await);
        assert!(
            message.contains("did not answer the listing within 10s"),
            "{message}"
        );
        drop(rx);
    }

    #[test]
    fn prune_abandoned_drops_listings_whose_consumer_gave_up() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (request, rx) = listing_request("/srv");
        let _ = session.handle_request(request);
        assert_eq!(session.prune_abandoned(), 0);
        drop(rx);
        assert_eq!(session.prune_abandoned(), 1);
    }

    // --- relayed PATH_QUERY (L3 §5) ----------------------------------------

    fn path_request(root: &str) -> (RelayRequest, oneshot::Receiver<PathQueryResult>) {
        let (reply, rx) = oneshot::channel();
        (
            RelayRequest::PathQuery {
                root: root.to_owned(),
                query: "src".to_owned(),
                recursive: true,
                reply,
            },
            rx,
        )
    }

    fn path_session() -> RelaySession {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        session
            .set_satellite_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]));
        session
    }

    fn path_error(result: PathQueryResult, root: &str) -> String {
        let refusal = result.expect_err("expected path refusal");
        assert_eq!(refusal.code, PathErrorCode::Other);
        assert_eq!(refusal.root, root);
        assert!(refusal.message.contains("devbox"));
        refusal.message
    }

    #[test]
    fn path_query_uses_shared_id_and_returns_satellite_results_without_retagging() {
        let mut session = path_session();
        let _ = session.handle_request(RelayRequest::Command {
            command: Command::Upgrade,
            reply: oneshot::channel().0,
            subscribe: None,
        });
        let (request, mut rx) = path_request("~/work");
        let FrameKind::PathQuery {
            request_id,
            root,
            query,
            recursive,
            host: forwarded_host,
        } = decode(&session.handle_request(request))
        else {
            panic!("expected PATH_QUERY");
        };
        assert_eq!(request_id, 2);
        assert_eq!(root, "~/work");
        assert_eq!(query, "src");
        assert!(recursive);
        assert_eq!(forwarded_host, None, "never chain a satellite route");
        let results = phux_protocol::wire::frame::PathResults {
            root: "/home/user/work".to_owned(),
            parent: Some("/home/user".to_owned()),
            rows: vec![],
            status: phux_protocol::wire::frame::PathStatus::Complete,
        };
        session
            .handle_inbound(&encode(&FrameKind::PathResults {
                request_id,
                result: Ok(results.clone()),
            }))
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), Ok(results));
        assert_eq!(
            session.pending_request_count(),
            1,
            "other command remains pending"
        );
    }

    #[test]
    fn older_satellite_refuses_path_query_before_forwarding() {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let (request, mut rx) = path_request("~/work");
        assert!(session.handle_request_checked(request).is_none());
        assert!(path_error(rx.try_recv().unwrap(), "~/work").contains("PATH_QUERY"));
        assert_eq!(session.pending_request_count(), 0);
    }

    #[test]
    fn path_query_correlated_error_and_disconnect_are_typed_refusals() {
        let mut session = path_session();
        let (request, mut rx) = path_request("/first");
        let FrameKind::PathQuery { request_id, .. } = decode(&session.handle_request(request))
        else {
            panic!("expected PATH_QUERY");
        };
        session
            .handle_inbound(&encode(&FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::InvalidCommand,
                message: "denied".to_owned(),
            }))
            .unwrap();
        assert!(path_error(rx.try_recv().unwrap(), "/first").contains("denied"));

        let (request, mut rx) = path_request("/second");
        let _ = session.handle_request(request);
        session.teardown("link lost");
        assert!(path_error(rx.try_recv().unwrap(), "/second").contains("link lost"));
    }

    #[test]
    fn satellite_path_refusal_preserves_its_specific_error_code() {
        let mut session = path_session();
        let (request, mut rx) = path_request("/private");
        let FrameKind::PathQuery { request_id, .. } = decode(&session.handle_request(request))
        else {
            panic!("expected PATH_QUERY");
        };
        let refusal = PathQueryError {
            root: "/private".to_owned(),
            code: PathErrorCode::PermissionDenied,
            message: "access denied".to_owned(),
        };
        session
            .handle_inbound(&encode(&FrameKind::PathResults {
                request_id,
                result: Err(refusal.clone()),
            }))
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), Err(refusal));
    }

    #[test]
    fn abandoned_path_query_is_pruned_and_late_results_are_dropped() {
        let mut session = path_session();
        let (request, rx) = path_request("/tmp");
        let FrameKind::PathQuery { request_id, .. } = decode(&session.handle_request(request))
        else {
            panic!("expected PATH_QUERY");
        };
        assert_eq!(session.prune_abandoned(), 0);
        drop(rx);
        assert_eq!(session.prune_abandoned(), 1);
        session
            .handle_inbound(&encode(&FrameKind::PathResults {
                request_id,
                result: Err(path_refusal("/tmp", "late")),
            }))
            .unwrap();
    }

    #[tokio::test]
    async fn disconnected_path_query_fails_fast_with_host_and_root() {
        let (request, rx) = path_request("/tmp");
        fail_fast(request, &host(), "backoff");
        assert!(path_error(rx.await.unwrap(), "/tmp").contains("backoff"));
    }

    #[tokio::test(start_paused = true)]
    async fn silent_path_query_times_out() {
        let (handle, _mailbox) = RelayHandle::new(host());
        assert!(
            path_error(
                handle
                    .path_query("~/work".to_owned(), "src".to_owned(), true)
                    .await,
                "~/work"
            )
            .contains("within 10s")
        );
    }

    #[tokio::test]
    async fn saturated_and_closed_path_query_mailboxes_refuse() {
        let (handle, mut mailbox) = RelayHandle::new(host());
        for _ in 0..RELAY_MAILBOX {
            handle.forward(FrameKind::Ping { nonce: 1 });
        }
        assert!(
            path_error(
                handle
                    .path_query("/tmp".to_owned(), String::new(), false)
                    .await,
                "/tmp"
            )
            .contains("saturated")
        );
        mailbox.requests.close();
        while mailbox.requests.try_recv().is_ok() {}
        drop(mailbox);
        assert!(
            path_error(
                handle
                    .path_query("/tmp".to_owned(), String::new(), false)
                    .await,
                "/tmp"
            )
            .contains("down")
        );
    }
}
