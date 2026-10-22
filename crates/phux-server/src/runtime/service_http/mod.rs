//! HTTP exchanges with a named service on this host (`SERVICE_HTTP_*`,
//! behind `features_ext.HOST_SERVICE_HTTP`).
//!
//! The one service is `opencode`, the host's `OpenCode` background service.
//! The server owns its origin and credential; the client sends
//! a request and pulls the body. All three commands are `INPUT` on the
//! server: a consumer that can type into a shell here can already run
//! `OpenCode`. A request naming a satellite is relayed to it, and the hub maps
//! the satellite's exchange to one of its own.
//!
//! Every command is admitted on the dispatcher (a slot reserved, a body
//! leased, a slot removed) and then runs on its own task, so neither a slow
//! open nor a read held open for an event stream delays other commands or
//! occupies the bulk lane. The connection's exchange slots bound that work:
//! one open or one read per slot.

#![expect(
    clippy::redundant_pub_crate,
    reason = "the connection dispatcher and the hub's relay admission reach this facade"
)]

mod exchanges;
mod opencode;
#[cfg(test)]
mod tests;
mod upstream;

use std::time::Duration;

use phux_protocol::ids::SatelliteHost;
use phux_protocol::service_http::{
    ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest, head_fits,
};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};

pub(crate) use exchanges::Exchanges;
use exchanges::{Lease, Reservation, Upstream};
use opencode::OpenCode;
use upstream::{BodyReader, Prepared};

use tokio_util::sync::CancellationToken;

use super::commands::CommandContext;
use crate::state::{Outbound, SharedState};

/// How long one read waits for body bytes before answering with none.
const READ_IDLE: Duration = Duration::from_secs(20);
/// The whole of one local open, discovery and retries included. Below the
/// hub's relay deadline, so a hub hears every satellite's answer.
const OPEN_DEADLINE: Duration = Duration::from_secs(25);
/// How often a connection with exchanges checks for abandoned ones.
const REAP_INTERVAL: Duration = Duration::from_secs(15);

const _: () = assert!(OPEN_DEADLINE.as_secs() < crate::hub::relay::RELAY_COMMAND_TIMEOUT.as_secs());

static OPENCODE: OpenCode = OpenCode::installed();

/// The services this server names.
fn provider(service: &str) -> Option<&'static OpenCode> {
    (service == "opencode").then_some(&OPENCODE)
}

/// A typed refusal: the command's `COMMAND_RESULT` error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Refusal {
    code: ErrorCode,
    message: String,
}

impl Refusal {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unavailable, message)
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidCommand, message)
    }

    fn busy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ResourceExhausted, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, message)
    }

    /// A satellite's answer that is not the expected value.
    fn from_reply(result: CommandResult) -> Self {
        match result {
            CommandResult::Error { code, message } => Self { code, message },
            other => Self::internal(format!("unexpected satellite reply: {other:?}")),
        }
    }

    fn into_result(self) -> CommandResult {
        CommandResult::Error {
            code: self.code,
            message: self.message,
        }
    }
}

/// Whether `command` runs here rather than in the command handler.
pub(crate) const fn is_service_http(command: &Command) -> bool {
    matches!(
        command,
        Command::ServiceHttpOpen { .. }
            | Command::ServiceHttpRead { .. }
            | Command::ServiceHttpClose { .. }
    )
}

/// Admit `command` against `exchanges` now; an admitted open or read then
/// runs on its own task and answers `request_id` itself.
pub(in crate::runtime) async fn submit(
    ctx: CommandContext,
    exchanges: &Exchanges,
    request_id: u32,
    command: Command,
) {
    let out_tx = ctx.out_tx.clone();
    if let Some(result) = admit(ctx, exchanges, request_id, command) {
        send_result(&out_tx, request_id, result).await;
    }
}

/// Close every exchange of an ending connection.
pub(in crate::runtime) fn close_all(state: &SharedState, exchanges: &Exchanges) {
    release_all(state, exchanges.close_all());
}

/// `Some` is the answer now; `None` means a spawned task answers.
fn admit(
    ctx: CommandContext,
    exchanges: &Exchanges,
    request_id: u32,
    command: Command,
) -> Option<CommandResult> {
    let admitted = match command {
        Command::ServiceHttpOpen { request } => {
            admit_open(exchanges, *request).map(|(reservation, target)| {
                spawn_open(ctx, exchanges, request_id, reservation, target);
            })
        }
        Command::ServiceHttpRead {
            exchange,
            max_bytes,
        } => admit_read(exchanges, exchange, max_bytes)
            .map(|lease| spawn_read(ctx, request_id, lease, max_bytes)),
        Command::ServiceHttpClose { exchange } => {
            release_all(&ctx.state, exchanges.close(exchange));
            return Some(CommandResult::Ok);
        }
        _ => Err(Refusal::internal("not a service HTTP command")),
    };
    admitted.err().map(Refusal::into_result)
}

/// Where an admitted open goes.
#[derive(Debug)]
enum Target {
    Local {
        provider: &'static OpenCode,
        prepared: Prepared,
    },
    Satellite {
        host: SatelliteHost,
        request: ServiceHttpRequest,
    },
}

/// Validate a local request (a satellite validates its own) and reserve a
/// slot that retains its body.
fn admit_open(
    exchanges: &Exchanges,
    request: ServiceHttpRequest,
) -> Result<(Reservation, Target), Refusal> {
    let target = open_target(request)?;
    let retained = match &target {
        Target::Local { prepared, .. } => prepared.body_len(),
        Target::Satellite { request, .. } => request.body.len(),
    };
    Ok((exchanges.reserve(retained)?, target))
}

fn open_target(mut request: ServiceHttpRequest) -> Result<Target, Refusal> {
    if let Some(host) = request.host.take() {
        return Ok(Target::Satellite {
            host: SatelliteHost::new(host),
            request,
        });
    }
    let provider = provider(&request.service).ok_or_else(|| {
        Refusal::not_found(format!(
            "this host serves no service named {:?} (known: opencode)",
            request.service
        ))
    })?;
    let prepared = Prepared::new(
        &request.method,
        &request.target,
        &request.headers,
        request.body,
    )?;
    if !OpenCode::proxies(prepared.path()) {
        return Err(Refusal::invalid(format!(
            "{:?} is OpenCode's pairing endpoint, which this server keeps to itself",
            prepared.path()
        )));
    }
    Ok(Target::Local { provider, prepared })
}

fn admit_read(exchanges: &Exchanges, exchange: u64, max_bytes: u32) -> Result<Lease, Refusal> {
    if max_bytes == 0 {
        return Err(Refusal::invalid(
            "a service HTTP read must ask for at least one byte",
        ));
    }
    exchanges.lease(exchange)
}

/// Run an open; the connection's cancellation drops it and its slot. The
/// first open starts the connection's reaper.
fn spawn_open(
    ctx: CommandContext,
    exchanges: &Exchanges,
    request_id: u32,
    reservation: Reservation,
    target: Target,
) {
    if exchanges.claim_reaper() {
        spawn_reaper(ctx.state.clone(), ctx.token.clone(), exchanges.clone());
    }
    tokio::task::spawn_local(async move {
        let token = ctx.token.clone();
        let result = tokio::select! {
            biased;
            () = token.cancelled() => return,
            opened = open(&ctx.state, reservation, target) => opened,
        };
        send_result(&ctx.out_tx, request_id, result).await;
    });
}

/// Close abandoned exchanges every [`REAP_INTERVAL`] until the connection
/// ends or holds none; the next open starts another reaper.
fn spawn_reaper(state: SharedState, token: CancellationToken, exchanges: Exchanges) {
    tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                biased;
                () = token.cancelled() => break,
                () = tokio::time::sleep(REAP_INTERVAL) => {}
            }
            // Tokio's clock, so a paused test clock drives it too.
            release_all(
                &state,
                exchanges.reap(tokio::time::Instant::now().into_std()),
            );
            if exchanges.is_empty() {
                break;
            }
        }
        exchanges.release_reaper();
    });
}

/// Run a read to its end: the exchange's close or the idle deadline ends
/// it, so the lease always finishes.
fn spawn_read(ctx: CommandContext, request_id: u32, lease: Lease, max_bytes: u32) {
    tokio::task::spawn_local(async move {
        let result = read(&ctx.state, lease, max_bytes, READ_IDLE).await;
        send_result(&ctx.out_tx, request_id, result).await;
    });
}

async fn open(state: &SharedState, reservation: Reservation, target: Target) -> CommandResult {
    let opened = match target {
        Target::Local { provider, prepared } => {
            open_local(provider, &prepared, OPEN_DEADLINE).await
        }
        Target::Satellite { host, request } => open_remote(state, host, request).await,
    };
    match opened {
        Ok((mut head, upstream)) => {
            head.exchange = reservation.exchange();
            if let Err(upstream) = reservation.fulfill(upstream) {
                release(state, upstream);
            }
            CommandResult::OkWith(CommandValue::ServiceHttpHead(head))
        }
        Err(refusal) => refusal.into_result(),
    }
}

/// The response head (its exchange still to be assigned) and body.
async fn open_local(
    provider: &OpenCode,
    prepared: &Prepared,
    deadline: Duration,
) -> Result<(ServiceHttpHead, Upstream), Refusal> {
    let (parts, body) = upstream::open(provider, prepared, deadline)
        .await?
        .into_parts();
    let head = ServiceHttpHead {
        exchange: 0,
        status: parts.status.as_u16(),
        headers: upstream::response_headers(&parts.headers),
    };
    if !head_fits(&head) {
        return Err(Refusal::internal(
            "the service's response head exceeds the SERVICE_HTTP bounds",
        ));
    }
    Ok((head, Upstream::Local(Box::new(BodyReader::new(body)))))
}

async fn open_remote(
    state: &SharedState,
    host: SatelliteHost,
    request: ServiceHttpRequest,
) -> Result<(ServiceHttpHead, Upstream), Refusal> {
    let relay = satellite(state, &host)?;
    let forwarded = Command::ServiceHttpOpen {
        request: Box::new(request),
    };
    let head = match relay.command(forwarded).await {
        CommandResult::OkWith(CommandValue::ServiceHttpHead(head)) => head,
        other => return Err(Refusal::from_reply(other)),
    };
    let upstream = Upstream::Remote {
        host,
        exchange: head.exchange,
    };
    Ok((head, upstream))
}

/// Read the next bytes. The chunk that ends the body also ends the
/// exchange (a satellite ends its own); an error or a close during the read
/// ends it and releases its upstream.
async fn read(
    state: &SharedState,
    mut lease: Lease,
    max_bytes: u32,
    idle: Duration,
) -> CommandResult {
    let closed = lease.closed();
    let exchange = lease.exchange();
    let outcome = tokio::select! {
        biased;
        () = closed.cancelled() => Err(exchanges::not_open(exchange)),
        outcome = read_upstream(state, &mut lease.upstream, max_bytes, idle) => outcome,
    };
    let ended = matches!(&outcome, Ok(chunk) if chunk.end);
    let released = lease.finish(outcome.is_ok() && !ended);
    if let Some(upstream) = released.filter(|_| !ended) {
        release(state, upstream);
    }
    outcome.map_or_else(Refusal::into_result, |chunk| {
        CommandResult::OkWith(CommandValue::ServiceHttpChunk(chunk))
    })
}

async fn read_upstream(
    state: &SharedState,
    upstream: &mut Upstream,
    max_bytes: u32,
    idle: Duration,
) -> Result<ServiceHttpChunk, Refusal> {
    match upstream {
        Upstream::Local(reader) => {
            let max = usize::try_from(max_bytes).unwrap_or(usize::MAX);
            reader.read(max, idle).await
        }
        Upstream::Remote { host, exchange } => {
            let relay = satellite(state, host)?;
            let read = Command::ServiceHttpRead {
                exchange: *exchange,
                max_bytes,
            };
            match relay.command(read).await {
                CommandResult::OkWith(CommandValue::ServiceHttpChunk(chunk)) => Ok(chunk),
                other => Err(Refusal::from_reply(other)),
            }
        }
    }
}

fn satellite(
    state: &SharedState,
    host: &SatelliteHost,
) -> Result<crate::hub::relay::RelayHandle, Refusal> {
    state.with(|s| s.hub_relay(host)).ok_or_else(|| {
        Refusal::new(
            ErrorCode::UnsupportedSatelliteRoute,
            format!("no satellite route to {host}: this server is not a federation hub for it"),
        )
    })
}

fn release_all(state: &SharedState, upstreams: impl IntoIterator<Item = Upstream>) {
    for upstream in upstreams {
        release(state, upstream);
    }
}

/// Let go of a closed exchange's upstream: a local body closes its
/// connection on drop; a satellite's exchange is closed there.
fn release(state: &SharedState, upstream: Upstream) {
    let Upstream::Remote { host, exchange } = upstream else {
        return;
    };
    let Ok(relay) = satellite(state, &host) else {
        return;
    };
    tokio::task::spawn_local(async move {
        let _ = relay.command(Command::ServiceHttpClose { exchange }).await;
    });
}

async fn send_result(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    result: CommandResult,
) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result,
        }))
        .await;
}
