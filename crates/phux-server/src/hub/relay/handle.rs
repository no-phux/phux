//! The producer handle a consumer path uses to reach one satellite relay.

use super::{
    Arc, AtomicU64, BootstrapProfile, ClientId, Command, CommandResult, DirectoryListingResult,
    ErrorCode, FrameKind, Mutex, Ordering, Outbound, PathQueryResult, ProxySubscription,
    RELAY_COMMAND_TIMEOUT, RELAY_LIST_DEADLINE, RELAY_MAILBOX, RelayCatalog, RelayHandle,
    RelayMailbox, RelayRequest, SatelliteHost, SatelliteSpawn, SpawnError, SpawnResult,
    Unsubscribe, WithdrawalReceipt, debug, listing_refusal, lock_withdrawal_outcome, mpsc, oneshot,
    path_refusal, trace, warn,
};

impl RelayHandle {
    /// Pair a fresh handle with the mailbox its link supervisor drains.
    pub(crate) fn new(host: SatelliteHost) -> (Self, RelayMailbox) {
        let (tx, rx) = mpsc::channel(RELAY_MAILBOX);
        let (unsub_tx, unsub_rx) = mpsc::unbounded_channel();
        let resource_kinds = Arc::new(Mutex::new(RelayCatalog::default()));
        (
            Self {
                host,
                tx,
                unsub_tx,
                seq: Arc::new(AtomicU64::new(1)),
                resource_kinds: Arc::clone(&resource_kinds),
            },
            RelayMailbox {
                requests: rx,
                unsubscribes: unsub_rx,
                journal: None,
                operations: crate::hub::operation_fence::OperationFence::default(),
                resource_kinds,
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

    pub(crate) fn known_resource_kind(
        &self,
        terminal: u32,
    ) -> Option<phux_protocol::ids::ResourceKind> {
        self.resource_kinds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .kinds
            .get(&terminal)
            .copied()
    }

    pub(crate) fn needs_kind_for_profile(&self, profile: BootstrapProfile) -> bool {
        self.resource_kinds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .terminal_profile
            .is_some_and(|selected| selected != profile)
    }

    /// Read the connection's known kind, falling back to the existing bounded
    /// inventory request for a direct target never previously enumerated.
    pub(crate) async fn resource_kind(
        &self,
        terminal: u32,
    ) -> Result<phux_protocol::ids::ResourceKind, CommandResult> {
        if let Some(kind) = self.known_resource_kind(terminal) {
            return Ok(kind);
        }
        match self
            .command(Command::GetState {
                scope: phux_protocol::wire::frame::StateScope::Server,
            })
            .await
        {
            CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(_)) => self
                .known_resource_kind(terminal)
                .ok_or_else(|| CommandResult::Error {
                    code: ErrorCode::TerminalNotFound,
                    message: format!("unknown satellite resource: {terminal}"),
                }),
            refusal => Err(refusal),
        }
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
        if crate::hub::operation_fence::fenced_key(&command).is_none() {
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

pub(super) const fn trysend_reason(err: &mpsc::error::TrySendError<RelayRequest>) -> &'static str {
    match err {
        mpsc::error::TrySendError::Full(_) => "mailbox full",
        mpsc::error::TrySendError::Closed(_) => "link task gone",
    }
}

pub(super) const fn forward_drop_category(frame: &FrameKind) -> &'static str {
    match frame {
        FrameKind::InputKey { .. }
        | FrameKind::InputMouse { .. }
        | FrameKind::InputFocus { .. }
        | FrameKind::InputPaste { .. } => "input_unacknowledged",
        FrameKind::FrameAck { .. } => "state_ack",
        _ => "uncorrelated_control",
    }
}
