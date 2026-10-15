//! Exclusive controller for a precreated SDK-owned `AgentSession`.
//!
//! Receive has its own connection; a held receive cannot block publication or
//! completion. Re-registration fences old generations without spawning a worker.
//! Publications retain bounded unacknowledged payloads and retry their original
//! identities. This module never submits or replays a provider prompt.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use phux_protocol::ResourceId;
use phux_protocol::native_agent::{
    MAX_NATIVE_PENDING, NativeCommand, NativeCommandResult, NativePublicationReceipt,
    NativeRequest, NativeRequestPayload, NativeUpdate,
};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, watch};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;

/// Native controller failures, keeping transport ambiguity separate from refusal.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket failed; publication/completion can retry the same payload.
    #[error("native controller transport: {0}")]
    Transport(#[source] AttachError),
    /// A publication, completion, or close was temporarily refused by a sealed
    /// or capacity-bound actor. Retry the exact payload after backoff.
    #[error("native controller mutation temporarily busy: {message}")]
    Busy {
        /// The broker's unchanged transient explanation.
        message: String,
    },
    /// The broker refused this exact request. Do not treat it as a disconnect.
    #[error("native controller refused ({code:?}): {message}")]
    Refused {
        /// The broker's structured refusal code.
        code: ErrorCode,
        /// The broker's unchanged explanation.
        message: String,
    },
    /// This controller or its resource has closed permanently.
    #[error("native controller closed: {message}")]
    Closed {
        /// The local or broker close explanation.
        message: String,
    },
    /// The request is invalid or the bounded pending publication queue is full.
    #[error("invalid native controller request: {0}")]
    Invalid(String),
    /// An unexpected response; acceptance cannot be inferred from it.
    #[error("native controller protocol: {0}")]
    Protocol(String),
}

impl ClientError {
    /// Transport loss or transient controller backpressure permits an identical retry.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Busy { .. })
    }
}

struct PendingPublication {
    id: u64,
    request: NativeRequestPayload,
}

struct ControlLane {
    connection: Option<Connection>,
    next_request: u32,
    next_publication: Option<u64>,
    pending: VecDeque<PendingPublication>,
}

impl ControlLane {
    fn request_id(&mut self) -> u32 {
        let id = self.next_request;
        self.next_request = id.wrapping_add(1).max(1);
        id
    }
}

#[derive(Default)]
struct ReceiveLane {
    connection: Option<Connection>,
    generation: u64,
    next_request: u32,
}

/// Host-only binding to an existing resource; it carries no create authority.
///
/// Share this value through `Arc` to drain commands concurrently with SDK output.
/// A failed publication remains pending (at most `MAX_NATIVE_PENDING` payloads).
/// Calling `publish` with that same update retries its original publication id;
/// different updates are ordered behind it. Transport methods try one reconnect
/// before returning an error. Refusals and closed resources never auto-reconnect.
/// `Busy` means mutation backpressure: back off and retry the same update,
/// result, or close. Keep the connection unless it drops; never repeat an SDK call.
pub struct NativeController {
    socket: PathBuf,
    agent: ResourceId,
    activation: String,
    binding_proof: String,
    generation: watch::Sender<u64>,
    control: Mutex<ControlLane>,
    receive: Mutex<ReceiveLane>,
    closed: AtomicBool,
}

impl std::fmt::Debug for NativeController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeController")
            .field("agent", &self.agent)
            .field("activation", &self.activation)
            .field("generation", &*self.generation.borrow())
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    generation: u64,
}

impl NativeController {
    /// Authenticate and register the same precreated resource and activation.
    ///
    /// # Errors
    /// Returns typed local validation, transport, broker refusal, or protocol errors.
    pub async fn connect(
        socket: impl AsRef<Path>,
        precreated_agent: ResourceId,
        activation: String,
        binding_proof: String,
    ) -> Result<Self, ClientError> {
        if activation.is_empty() || binding_proof.is_empty() {
            return Err(ClientError::Invalid(
                "activation and binding proof are required".into(),
            ));
        }
        let request = NativeRequest::Register {
            agent_session_id: precreated_agent.clone(),
            activation: activation.clone(),
            binding_proof: binding_proof.clone(),
        };
        let request = NativeRequestPayload::new(request).map_err(ClientError::Invalid)?;
        let (connection, generation) = register_connection(socket.as_ref(), 1, request).await?;
        let (generation, _) = watch::channel(generation);
        Ok(Self {
            socket: socket.as_ref().to_owned(),
            agent: precreated_agent,
            activation,
            binding_proof,
            generation,
            control: Mutex::new(ControlLane {
                connection: Some(connection),
                next_request: 2,
                next_publication: Some(1),
                pending: VecDeque::new(),
            }),
            receive: Mutex::new(ReceiveLane::default()),
            closed: AtomicBool::new(false),
        })
    }

    /// Hold a receive on an independent authenticated connection.
    ///
    /// # Errors
    /// Retries one transport loss by re-registering; never executes an intent.
    /// Refusals, closure, and invalid command responses return immediately.
    pub async fn receive(&self) -> Result<NativeCommand, ClientError> {
        let mut lane = self.receive.lock().await;
        let mut generations = self.generation.subscribe();
        let mut retried = false;
        loop {
            self.ensure_open()?;
            {
                let mut control = self.control.lock().await;
                self.ensure_connected(&mut control).await?;
            }
            let generation = *generations.borrow_and_update();
            if lane.generation != generation {
                lane.connection = None;
                lane.generation = generation;
            }
            let Some(mut connection) = self
                .receive_connection(&mut lane, generation, &mut retried)
                .await?
            else {
                continue;
            };
            let request = NativeRequestPayload::new(NativeRequest::Receive {
                agent_session_id: self.agent.clone(),
                activation: self.activation.clone(),
                binding_proof: self.binding_proof.clone(),
                generation,
            })
            .map_err(ClientError::Invalid)?;
            let request_id = lane.next_request.max(1);
            lane.next_request = request_id.wrapping_add(1).max(1);
            // Registration fences an outstanding receive. Never reuse a socket
            // with an unread acknowledgment after cancellation/generation change.
            let result = tokio::select! {
                result = round_trip(&mut connection, request_id, request) => result,
                _ = generations.changed() => { drop(connection); continue; },
            };
            match result {
                Ok(result) => {
                    if let Some(command) =
                        self.accept_received(&mut lane, connection, generation, result)?
                    {
                        drop(lane);
                        return Ok(command);
                    }
                }
                Err(error) => {
                    drop(connection);
                    self.retry_receive(error, generation, &mut retried).await?;
                }
            }
        }
    }

    async fn receive_connection(
        &self,
        lane: &mut ReceiveLane,
        generation: u64,
        retried: &mut bool,
    ) -> Result<Option<Connection>, ClientError> {
        if let Some(connection) = lane.connection.take() {
            return Ok(Some(connection));
        }
        match Connection::connect(&self.socket).await {
            Ok(connection) => Ok(Some(connection)),
            Err(error) => {
                let error = transport_error(error);
                if error.is_retryable() && !*retried {
                    *retried = true;
                    self.reconnect_generation(generation).await?;
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }

    fn accept_received(
        &self,
        lane: &mut ReceiveLane,
        connection: Connection,
        generation: u64,
        result: CommandResult,
    ) -> Result<Option<NativeCommand>, ClientError> {
        self.ensure_open()?;
        if *self.generation.borrow() != generation {
            return Ok(None);
        }
        let command = self.received_command(result)?;
        lane.connection = Some(connection);
        Ok(Some(command))
    }

    fn received_command(&self, result: CommandResult) -> Result<NativeCommand, ClientError> {
        let command: NativeCommand = json_reply(result)?;
        if command.activation != self.activation || command.operation_id == 0 {
            return Err(ClientError::Protocol(
                "received command identity does not match the binding".into(),
            ));
        }
        command.intent.validate().map_err(ClientError::Protocol)?;
        Ok(command)
    }

    async fn retry_receive(
        &self,
        error: ClientError,
        generation: u64,
        retried: &mut bool,
    ) -> Result<(), ClientError> {
        if error.is_retryable() && !*retried {
            *retried = true;
            self.reconnect_generation(generation).await?;
            return Ok(());
        }
        if matches!(
            error,
            ClientError::Refused {
                code: ErrorCode::PreconditionFailed,
                ..
            }
        ) {
            // The broker may fence Receive before Register's reply reaches the
            // control socket. Wait for that registration, never replay input.
            let control = self.control.lock().await;
            let changed = *self.generation.borrow() != generation;
            drop(control);
            if changed {
                return Ok(());
            }
        }
        if *self.generation.borrow() != generation {
            return Ok(());
        }
        Err(self.record_error(error))
    }

    /// Complete a received operation with its factual SDK disposition.
    ///
    /// # Errors
    /// One transport retry preserves the exact result. No provider call is replayed.
    /// On Busy, the host's operation ledger must retain and retry this exact
    /// result after backoff; the SDK operation must not be executed again.
    pub async fn complete(&self, result: NativeCommandResult) -> Result<(), ClientError> {
        let mut lane = self.control.lock().await;
        self.ensure_open()?;
        let request = NativeRequestPayload::new(self.complete_request(result))
            .map_err(ClientError::Invalid)?;
        ok_reply(&self.control_request(&mut lane, request).await?)
    }

    /// Publish output in order, retaining its original id until acknowledged.
    ///
    /// # Errors
    /// A transport failure retains the payload for an identical retry, including
    /// after generation changes. A full queue or oversized payload is Invalid;
    /// a broker refusal is explicit and never represented as acceptance.
    /// Actor-seal/capacity Overflow is Busy: the pending identity and healthy
    /// connection are retained for a caller's backoff retry.
    pub async fn publish(
        &self,
        update: NativeUpdate,
    ) -> Result<NativePublicationReceipt, ClientError> {
        let mut lane = self.control.lock().await;
        self.ensure_open()?;
        let id = self.reserve_output(&mut lane, update)?;
        loop {
            let pending = lane
                .pending
                .front()
                .ok_or_else(|| ClientError::Protocol("pending publication disappeared".into()))?;
            let expected_id = pending.id;
            let request = pending.request.clone();
            let receipt: NativePublicationReceipt =
                json_reply(self.control_request(&mut lane, request).await?)?;
            if receipt.publication_id != expected_id || receipt.first_seq > receipt.last_seq {
                return Err(ClientError::Protocol(
                    "publication acknowledgement has invalid identity or sequence range".into(),
                ));
            }
            lane.pending.pop_front();
            if expected_id == id {
                drop(lane);
                return Ok(receipt);
            }
        }
    }

    fn reserve_output(
        &self,
        lane: &mut ControlLane,
        update: NativeUpdate,
    ) -> Result<u64, ClientError> {
        if let Some(pending) = lane.pending.iter().find(|pending| {
            matches!(
                pending.request.request(),
                NativeRequest::Publish { update: queued, .. } if *queued == update
            )
        }) {
            Ok(pending.id)
        } else {
            if lane.pending.len() >= MAX_NATIVE_PENDING {
                return Err(ClientError::Invalid(
                    "pending native publication queue is full".into(),
                ));
            }
            let id = lane
                .next_publication
                .ok_or_else(|| ClientError::Invalid("publication ids exhausted".into()))?;
            let request = NativeRequestPayload::new(self.publish_request(id, update))
                .map_err(ClientError::Invalid)?;
            lane.next_publication = id.checked_add(1);
            lane.pending.push_back(PendingPublication { id, request });
            Ok(id)
        }
    }

    /// Close this binding and wake any held receive. Idempotent after success.
    ///
    /// # Errors
    /// A failed close is not claimed successful; one transport retry is safe.
    /// On Busy, retry this binding after backoff. It remains open until an
    /// actual close acknowledgement; no new registration is necessary.
    pub async fn close(&self) -> Result<(), ClientError> {
        let mut lane = self.control.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let request = NativeRequestPayload::new(NativeRequest::Close {
            agent_session_id: self.agent.clone(),
            activation: self.activation.clone(),
            binding_proof: self.binding_proof.clone(),
            generation: *self.generation.borrow(),
        })
        .map_err(ClientError::Invalid)?;
        ok_reply(&self.control_request(&mut lane, request).await?)?;
        self.closed.store(true, Ordering::Release);
        lane.connection = None;
        self.generation.send_modify(|_| {});
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), ClientError> {
        if self.closed.load(Ordering::Acquire) {
            Err(ClientError::Closed {
                message: "binding is closed".into(),
            })
        } else {
            Ok(())
        }
    }

    fn record_error(&self, error: ClientError) -> ClientError {
        if matches!(error, ClientError::Closed { .. }) {
            self.closed.store(true, Ordering::Release);
            self.generation.send_modify(|_| {});
        }
        error
    }

    async fn ensure_connected(&self, lane: &mut ControlLane) -> Result<(), ClientError> {
        self.ensure_open()?;
        if lane.connection.is_none() {
            self.register(lane).await?;
        }
        Ok(())
    }

    async fn register(&self, lane: &mut ControlLane) -> Result<(), ClientError> {
        self.ensure_open()?;
        let request = NativeRequestPayload::new(NativeRequest::Register {
            agent_session_id: self.agent.clone(),
            activation: self.activation.clone(),
            binding_proof: self.binding_proof.clone(),
        })
        .map_err(ClientError::Invalid)?;
        lane.connection = None;
        let (connection, generation) =
            register_connection(&self.socket, lane.request_id(), request)
                .await
                .map_err(|error| self.record_error(error))?;
        lane.connection = Some(connection);
        self.generation.send_replace(generation);
        Ok(())
    }

    async fn reconnect_generation(&self, generation: u64) -> Result<(), ClientError> {
        let mut lane = self.control.lock().await;
        if *self.generation.borrow() == generation || lane.connection.is_none() {
            self.register(&mut lane).await?;
        }
        Ok(())
    }

    async fn control_request(
        &self,
        lane: &mut ControlLane,
        request: NativeRequestPayload,
    ) -> Result<CommandResult, ClientError> {
        self.ensure_connected(lane).await?;
        let request = set_generation(request, *self.generation.borrow())?;
        match self.control_once(lane, request.clone()).await {
            Err(ClientError::Transport(_)) => {
                self.register(lane).await?;
                let request = set_generation(request, *self.generation.borrow())?;
                self.control_once(lane, request).await
            }
            result => result,
        }
    }

    async fn control_once(
        &self,
        lane: &mut ControlLane,
        request: NativeRequestPayload,
    ) -> Result<CommandResult, ClientError> {
        let request_id = lane.request_id();
        // Taking ownership before awaiting makes cancellation safe: the
        // socket is dropped rather than reused with an unread acknowledgement.
        let mut connection = lane
            .connection
            .take()
            .ok_or_else(|| ClientError::Protocol("control connection unavailable".into()))?;
        match round_trip(&mut connection, request_id, request).await {
            Ok(result) => {
                lane.connection = Some(connection);
                Ok(result)
            }
            Err(error @ ClientError::Busy { .. }) => {
                // A correlated transient refusal consumed its reply cleanly.
                // Keep the socket and generation; callers back off, retaining
                // the publication instead of reconnecting into an upgrade loop.
                lane.connection = Some(connection);
                Err(error)
            }
            Err(error) => Err(self.record_error(error)),
        }
    }

    fn complete_request(&self, result: NativeCommandResult) -> NativeRequest {
        NativeRequest::Complete {
            agent_session_id: self.agent.clone(),
            activation: self.activation.clone(),
            binding_proof: self.binding_proof.clone(),
            generation: *self.generation.borrow(),
            result,
        }
    }

    fn publish_request(&self, publication_id: u64, update: NativeUpdate) -> NativeRequest {
        NativeRequest::Publish {
            agent_session_id: self.agent.clone(),
            activation: self.activation.clone(),
            binding_proof: self.binding_proof.clone(),
            generation: *self.generation.borrow(),
            publication_id,
            update,
        }
    }
}

fn set_generation(
    request: NativeRequestPayload,
    current: u64,
) -> Result<NativeRequestPayload, ClientError> {
    let generation = match request.request() {
        NativeRequest::Complete { generation, .. }
        | NativeRequest::Publish { generation, .. }
        | NativeRequest::Close { generation, .. } => *generation,
        _ => return Ok(request),
    };
    if generation == current {
        return Ok(request);
    }
    let mut request = request.into_request();
    match &mut request {
        NativeRequest::Complete { generation, .. }
        | NativeRequest::Publish { generation, .. }
        | NativeRequest::Close { generation, .. } => *generation = current,
        _ => {}
    }
    NativeRequestPayload::new(request).map_err(ClientError::Invalid)
}

fn transport_error(error: AttachError) -> ClientError {
    if error.is_fatal_refusal() {
        return ClientError::Refused {
            code: ErrorCode::PermissionDenied,
            message: error.to_string(),
        };
    }
    match error {
        AttachError::Protocol(message) => ClientError::Protocol(message),
        AttachError::Framing(error) => ClientError::Protocol(error.to_string()),
        AttachError::Refused(message) => ClientError::Refused {
            code: ErrorCode::PermissionDenied,
            message,
        },
        AttachError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ClientError::Refused {
                code: ErrorCode::PermissionDenied,
                message: error.to_string(),
            }
        }
        AttachError::Io(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            ClientError::Invalid(error.to_string())
        }
        error => ClientError::Transport(error),
    }
}

/// Return a usable transport only after validating the broker's generation.
async fn register_connection(
    socket: &Path,
    request_id: u32,
    request: NativeRequestPayload,
) -> Result<(Connection, u64), ClientError> {
    let mut connection = Connection::connect(socket).await.map_err(transport_error)?;
    let registration: Registration =
        json_reply(round_trip(&mut connection, request_id, request).await?)?;
    if registration.generation == 0 {
        return Err(ClientError::Protocol(
            "registration returned a zero generation".into(),
        ));
    }
    Ok((connection, registration.generation))
}

async fn round_trip(
    connection: &mut Connection,
    request_id: u32,
    request: NativeRequestPayload,
) -> Result<CommandResult, ClientError> {
    let mutation = matches!(
        request.request(),
        NativeRequest::Publish { .. }
            | NativeRequest::Complete { .. }
            | NativeRequest::Close { .. }
    );
    let (result, _interleaved) = connection
        .request(request_id, Command::NativeAgent { request })
        .await
        .map_err(transport_error)?
        .into_parts();
    // These connections subscribe to no resource output. Their only owned lane
    // is the correlated native reply; no transcript evidence is discarded here.
    match result {
        CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message,
        } => Err(ClientError::Closed { message }),
        CommandResult::Error {
            code: ErrorCode::Overflow,
            message,
        } if mutation => Err(ClientError::Busy { message }),
        CommandResult::Error { code, message } => Err(ClientError::Refused { code, message }),
        result => Ok(result),
    }
}

fn json_reply<T: DeserializeOwned>(result: CommandResult) -> Result<T, ClientError> {
    match result {
        CommandResult::OkWith(CommandValue::Json(json)) => serde_json::from_str(&json)
            .map_err(|error| ClientError::Protocol(format!("invalid native reply: {error}"))),
        _ => Err(ClientError::Protocol("expected a native JSON reply".into())),
    }
}

fn ok_reply(result: &CommandResult) -> Result<(), ClientError> {
    match result {
        CommandResult::Ok => Ok(()),
        _ => Err(ClientError::Protocol(
            "expected a native completion acknowledgement".into(),
        )),
    }
}
