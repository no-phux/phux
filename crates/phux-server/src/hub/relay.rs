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
//! **Directory listings** (`docs/protocol.md#commands-events-resources`) ride the link as
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
    PathQueryError, PathQueryResult, RolePolicy, Scope, SpawnError, SpawnResult, TombstoneReason,
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

/// Only identity/kind facts and the selected Terminal profile survive between
/// routing calls; they are cleared at every link incarnation boundary.
#[derive(Debug, Default)]
pub(crate) struct RelayCatalog {
    kinds: HashMap<u32, phux_protocol::ids::ResourceKind>,
    terminal_profile: Option<BootstrapProfile>,
}

pub(crate) type ResourceKindCatalog = Arc<Mutex<RelayCatalog>>;

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
    /// Known resource kind; `AgentSession` codecs are independent of the
    /// connection's negotiated Terminal profile.
    pub(crate) resource_kind: Option<phux_protocol::ids::ResourceKind>,
    /// The downstream connection's Terminal bootstrap selection. Terminal
    /// subscriptions must match the link exactly; `AgentSession` is kind-fixed.
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
    /// Relay a `LIST_DIRECTORY` (`docs/protocol.md#commands-events-resources`); `reply` resolves
    /// with the listing or a refusal naming the host.
    ListDirectory {
        /// The requested path, verbatim.
        path: String,
        /// Resolved with the listing or a typed refusal.
        reply: oneshot::Sender<DirectoryListingResult>,
    },
    /// A satellite-local path query; the session strips the hub host field
    /// and correlates `PATH_RESULTS` through a link-side request id.
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
    /// Kinds learned on this link, shared with its routing handle.
    pub(crate) resource_kinds: ResourceKindCatalog,
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
    resource_kinds: ResourceKindCatalog,
}

mod admission;
mod bootstrap;
mod construct;
mod fanout;
mod handle;
mod inbound;
mod lifecycle;
mod mirror;
pub mod relays;
mod replies;
mod requests;
mod retention;
pub mod routing;
pub mod state;
mod stream;
mod withdrawal;

pub(crate) use relays::*;
pub(crate) use routing::*;
pub(crate) use state::*;

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
