//! TCP port forwarding (`FORWARD_*`, behind `features_ext.FORWARD_TCP`;
//! `docs/forwarding.md`).
//!
//! The client asks this server to dial a TCP port, by default only on its
//! own loopback, then pulls and pushes the connection's bytes with
//! commands. All four commands are `INPUT` on the server: a consumer that
//! can type into a shell here can already open the same connection.
//!
//! Every command is admitted on the dispatcher (a slot reserved, a half
//! leased, a slot removed) and then runs on its own task, so neither a
//! slow dial, a read parked on a quiet connection, nor a write the target
//! is slow to take delays other commands. A write is answered only after
//! `write_all`, which is the client's backpressure.

#![expect(
    clippy::redundant_pub_crate,
    reason = "the connection dispatcher reaches this facade"
)]

pub(crate) mod agent;
mod slots;
#[cfg(test)]
mod tests;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use phux_protocol::forward::{ForwardChunk, ForwardOp};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::sync::CancellationToken;

pub(crate) use slots::Forwards;
use slots::{Closed, Lease, Reservation};

use super::commands::CommandContext;
use crate::state::{ClientId, Outbound, SharedState};

/// How long one read waits for bytes before answering with none.
const READ_IDLE: Duration = Duration::from_secs(20);
/// The whole of one dial, both loopback families included.
const DIAL_DEADLINE: Duration = Duration::from_secs(10);
/// How often a connection with forwards checks for abandoned ones.
const REAP_INTERVAL: Duration = Duration::from_secs(15);

fn refuse(code: ErrorCode, message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.into(),
    }
}

/// Whether `command` runs here rather than in the command handler.
pub(crate) const fn is_forward(command: &Command) -> bool {
    matches!(command, Command::Forward { .. })
}

/// Admit `command` against `forwards` now; an admitted open, read, or
/// write then runs on its own task and answers `request_id` itself.
pub(in crate::runtime) async fn submit(
    ctx: CommandContext,
    forwards: &Forwards,
    request_id: u32,
    command: Command,
) {
    let out_tx = ctx.out_tx.clone();
    if let Some(result) = admit(ctx, forwards, request_id, command) {
        send_result(&out_tx, request_id, result).await;
    }
}

/// Close every forward of an ending connection, and withdraw its
/// ssh-agent offer.
pub(in crate::runtime) fn close_all(state: &SharedState, client: ClientId, forwards: &Forwards) {
    for closed in forwards.close_all() {
        audit_close(state, client, &closed, "disconnected");
    }
    agent::withdraw(state, client);
}

/// `Some` is the answer now; `None` means a spawned task answers.
fn admit(
    ctx: CommandContext,
    forwards: &Forwards,
    request_id: u32,
    command: Command,
) -> Option<CommandResult> {
    let Command::Forward { op } = command else {
        return Some(refuse(ErrorCode::InternalError, "not a forward command"));
    };
    if matches!(op, ForwardOp::AgentOffer | ForwardOp::AgentReply { .. }) {
        return Some(agent::admit(&ctx, op));
    }
    let (enabled, allow) = ctx.state.with(|s| {
        let cfg = s.forward();
        let allow = matches!(op, ForwardOp::Open { .. }).then(|| cfg.allow.clone());
        (cfg.tcp, allow.unwrap_or_default())
    });
    if !enabled {
        return Some(refuse(
            ErrorCode::Unsupported,
            "TCP forwarding is turned off on this server (`[forward] tcp = false`)",
        ));
    }
    let admitted = match op {
        ForwardOp::Open { addr, port } => admit_open(forwards, &addr, port, &allow)
            .map(|dial| spawn_open(ctx, forwards, request_id, dial)),
        ForwardOp::Read { id, max_bytes } => admit_read(forwards, id, max_bytes).map(|lease| {
            tokio::task::spawn_local(async move {
                let result = read(&ctx, lease, max_bytes, READ_IDLE).await;
                send_result(&ctx.out_tx, request_id, result).await;
            });
        }),
        ForwardOp::Write { id, bytes, fin } => forwards.lease_write(id).map(|lease| {
            tokio::task::spawn_local(async move {
                let result = write(&ctx, lease, &bytes, fin).await;
                send_result(&ctx.out_tx, request_id, result).await;
            });
        }),
        ForwardOp::Close { id } => {
            if let Some(closed) = forwards.close(id) {
                audit_close(&ctx.state, ctx.client_id, &closed, "closed");
            }
            return Some(CommandResult::Ok);
        }
        ForwardOp::AgentOffer | ForwardOp::AgentReply { .. } => return None,
    };
    admitted.err()
}

/// Where an admitted open dials.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// This machine's loopback, or an allow-listed address: tried in order.
    Addrs(Vec<SocketAddr>),
    /// An allow-listed `host:port` the server's own configuration names.
    Name(String),
}

/// The dial for `addr:port`, or the refusal. Nothing the client names is
/// ever looked up: `localhost` is this machine's two loopback addresses,
/// and any other name must be one `allow` spells out.
fn target(addr: &str, port: u16, allow: &[String]) -> Result<Target, CommandResult> {
    if port == 0 {
        return Err(refuse(ErrorCode::InvalidCommand, "a forward needs a port"));
    }
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    match addr {
        "localhost" => return Ok(Target::Addrs(vec![v4, v6])),
        "127.0.0.1" => return Ok(Target::Addrs(vec![v4])),
        "::1" => return Ok(Target::Addrs(vec![v6])),
        _ => {}
    }
    let literal = literal(addr, port);
    if !allow.contains(&literal) {
        return Err(refuse(
            ErrorCode::PermissionDenied,
            format!(
                "{literal} is not on this server's loopback: phux forwards only to localhost, \
                 127.0.0.1, or ::1 unless the server's `[forward] allow` lists the target"
            ),
        ));
    }
    Ok(literal
        .parse()
        .map_or(Target::Name(literal), |addr| Target::Addrs(vec![addr])))
}

/// `host:port`, with an IPv6 address bracketed, as `allow` spells a target.
fn literal(addr: &str, port: u16) -> String {
    if addr.contains(':') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    }
}

/// An admitted open: its slot, where it dials, and the name of that.
struct Dial {
    reservation: Reservation,
    target: Target,
    label: String,
}

fn admit_open(
    forwards: &Forwards,
    addr: &str,
    port: u16,
    allow: &[String],
) -> Result<Dial, CommandResult> {
    let target = target(addr, port, allow)?;
    let label = literal(addr, port);
    Ok(Dial {
        reservation: forwards.reserve(label.clone())?,
        target,
        label,
    })
}

fn admit_read(
    forwards: &Forwards,
    id: u64,
    max_bytes: u32,
) -> Result<Lease<OwnedReadHalf>, CommandResult> {
    if max_bytes == 0 {
        return Err(refuse(
            ErrorCode::InvalidCommand,
            "a forward read must ask for at least one byte",
        ));
    }
    forwards.lease_read(id)
}

/// Run a dial; the connection's cancellation drops it and its slot. The
/// first open starts the connection's reaper.
fn spawn_open(ctx: CommandContext, forwards: &Forwards, request_id: u32, dial: Dial) {
    if forwards.claim_reaper() {
        spawn_reaper(
            ctx.state.clone(),
            ctx.client_id,
            ctx.token.clone(),
            forwards.clone(),
        );
    }
    tokio::task::spawn_local(async move {
        let token = ctx.token.clone();
        let result = tokio::select! {
            biased;
            () = token.cancelled() => return,
            opened = open(&ctx, dial.reservation, dial.label, connect(dial.target), DIAL_DEADLINE) => opened,
        };
        send_result(&ctx.out_tx, request_id, result).await;
    });
}

/// Close abandoned forwards every [`REAP_INTERVAL`] until the connection
/// ends or holds none; the next open starts another reaper.
fn spawn_reaper(
    state: SharedState,
    client: ClientId,
    token: CancellationToken,
    forwards: Forwards,
) {
    tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                biased;
                () = token.cancelled() => break,
                () = tokio::time::sleep(REAP_INTERVAL) => {}
            }
            for closed in forwards.reap(tokio::time::Instant::now()) {
                audit_close(&state, client, &closed, "idle");
            }
            if forwards.is_empty() {
                break;
            }
        }
        forwards.release_reaper();
    });
}

/// Finish an admitted open once `connecting` does, or refuse it at
/// `deadline`.
async fn open(
    ctx: &CommandContext,
    reservation: Reservation,
    label: String,
    connecting: impl Future<Output = std::io::Result<TcpStream>>,
    deadline: Duration,
) -> CommandResult {
    let stream = match tokio::time::timeout(deadline, connecting).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            return refuse(
                ErrorCode::Unavailable,
                format!("could not connect to {label}: {err}"),
            );
        }
        Err(_) => {
            return refuse(
                ErrorCode::Unavailable,
                format!("no answer from {label} within {} s", deadline.as_secs()),
            );
        }
    };
    // Small interactive writes should not wait on Nagle behind the phux hop.
    let _ = stream.set_nodelay(true);
    let id = reservation.id();
    if !reservation.fulfill(stream) {
        return slots::not_open(id);
    }
    if crate::audit::enabled() {
        let actor = ctx.state.with(|s| s.audit_actor(ctx.client_id));
        crate::audit::emit(
            crate::audit::Kind::ForwardOpen,
            actor,
            "FORWARD_OPEN",
            Some(label),
            crate::audit::Outcome::Allowed,
            serde_json::json!({ "client": ctx.client_id.0, "forward": id }),
        );
    }
    CommandResult::OkWith(CommandValue::ForwardOpened(id))
}

async fn connect(target: Target) -> std::io::Result<TcpStream> {
    match target {
        Target::Name(name) => TcpStream::connect(name).await,
        Target::Addrs(addrs) => {
            let mut last = None;
            for addr in addrs {
                match TcpStream::connect(addr).await {
                    Ok(stream) => return Ok(stream),
                    Err(err) => last = Some(err),
                }
            }
            Err(last.unwrap_or_else(|| std::io::ErrorKind::AddrNotAvailable.into()))
        }
    }
}

/// Read what the target has sent, up to `max_bytes`: at least one byte,
/// the end, or, after `idle`, nothing. A close during the read ends it; a
/// failed read ends the forward.
async fn read(
    ctx: &CommandContext,
    mut lease: Lease<OwnedReadHalf>,
    max_bytes: u32,
    idle: Duration,
) -> CommandResult {
    let closed = lease.closed();
    let mut bytes = vec![0; usize::try_from(max_bytes).unwrap_or(usize::MAX)];
    let first = tokio::select! {
        biased;
        () = closed.cancelled() => return slots::not_open(lease.id()),
        first = tokio::time::timeout(idle, lease.half.read(&mut bytes)) => first,
    };
    let (mut filled, mut eof) = match first {
        Err(_) => (0, false),
        Ok(Ok(read)) => (read, read == 0),
        Ok(Err(err)) => return failed(ctx, lease, &err),
    };
    // Whatever else already arrived rides in the same answer.
    while filled > 0 && filled < bytes.len() && !eof {
        match lease.half.try_read(&mut bytes[filled..]) {
            Ok(0) => eof = true,
            Ok(read) => filled += read,
            Err(_) => break,
        }
    }
    bytes.truncate(filled);
    lease.finish(filled);
    CommandResult::OkWith(CommandValue::ForwardChunk(ForwardChunk { bytes, eof }))
}

/// Write all of `bytes` to the target, then with `fin` shut the write half
/// down. Answered only once that is done.
async fn write(
    ctx: &CommandContext,
    mut lease: Lease<OwnedWriteHalf>,
    bytes: &[u8],
    fin: bool,
) -> CommandResult {
    let closed = lease.closed();
    let written = tokio::select! {
        biased;
        () = closed.cancelled() => return slots::not_open(lease.id()),
        written = async {
            lease.half.write_all(bytes).await?;
            if fin {
                lease.half.shutdown().await?;
            }
            Ok(())
        } => written,
    };
    match written {
        Ok(()) => {
            lease.finish(bytes.len(), fin);
            CommandResult::Ok
        }
        Err(err) => failed(ctx, lease, &err),
    }
}

/// The target's side of `lease`'s forward failed: close it and say why.
fn failed<H>(ctx: &CommandContext, lease: Lease<H>, err: &std::io::Error) -> CommandResult {
    let id = lease.id();
    if let Some(closed) = lease.fail() {
        audit_close(&ctx.state, ctx.client_id, &closed, "error");
    }
    refuse(ErrorCode::Unavailable, format!("forward {id} ended: {err}"))
}

/// `forward.close`: the byte counts each way and why, never the bytes.
fn audit_close(state: &SharedState, client: ClientId, closed: &Closed, reason: &str) {
    if !crate::audit::enabled() {
        return;
    }
    let actor = state.with(|s| s.audit_actor(client));
    crate::audit::emit(
        crate::audit::Kind::ForwardClose,
        actor,
        "FORWARD_CLOSE",
        Some(closed.target.clone()),
        crate::audit::Outcome::Allowed,
        serde_json::json!({
            "client": client.0,
            "forward": closed.id,
            "sent": closed.sent,
            "received": closed.received,
            "reason": reason,
        }),
    );
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
