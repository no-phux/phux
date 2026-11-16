//! ssh-agent forwarding (`AGENT_OFFER`, `AGENT_REPLY`, behind
//! `features_ext.FORWARD_AGENT`; `docs/forwarding.md#ssh-agent`).
//!
//! The server serves an owner-only `SSH_AUTH_SOCK` but holds no key: list and
//! sign requests go, as unjournaled `agent_request` events, to the one client
//! that offered its agent; everything else gets `SSH_AGENT_FAILURE`. The
//! broker table lives in `ServerState`, so each step is one lock.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use phux_protocol::forward::{
    AGENT_REQUEST_SECS, MAX_AGENT_MESSAGE_BYTES, SSH_AGENT_FAILURE, agent_answer_type,
    sign_request_key,
};
use phux_protocol::wire::frame::{AgentEvent, CommandResult, ErrorCode, FrameKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::runtime::commands::CommandContext;
use crate::state::{ClientId, Outbound, SharedState};

/// Requests waiting on the offering client at once; one more is refused.
const MAX_PENDING: usize = 64;
/// Connections to the broker socket served at once; one more is closed.
// ponytail: a pane process that parks 64 idle connections starves the
// others on this server; add an idle deadline per connection if that is
// ever seen.
const MAX_CONNECTIONS: usize = 64;
/// How long a pane's request waits for the offering client.
pub(super) const REQUEST_DEADLINE: Duration = Duration::from_secs(AGENT_REQUEST_SECS);

/// The broker socket for the server listening at `server_socket`: beside
/// it, in the server's runtime directory.
fn socket_path_for(server_socket: &Path) -> PathBuf {
    let mut name = server_socket
        .file_name()
        .map_or_else(|| "phux.sock".into(), std::ffi::OsStr::to_os_string);
    name.push(".ssh-agent");
    server_socket.with_file_name(name)
}

/// The client serving its agent, and where its events go.
#[derive(Debug)]
struct Offer {
    client: ClientId,
    out_tx: mpsc::Sender<Outbound>,
}

/// One pane request waiting on `client`'s reply.
#[derive(Debug)]
struct Pending {
    client: ClientId,
    answer: oneshot::Sender<Vec<u8>>,
}

/// The server's ssh-agent broker: its socket, the current offer, and the
/// requests in flight.
#[derive(Debug, Default)]
pub(crate) struct Broker {
    socket: Option<PathBuf>,
    offer: Option<Offer>,
    pending: HashMap<u64, Pending>,
    /// Never reused within a server process.
    last_id: u64,
}

impl Broker {
    /// The bound broker socket: `Some` exactly when forwarding is on, which
    /// is what advertises the bit, admits the commands, and sets a new
    /// pane's `SSH_AUTH_SOCK`.
    pub(crate) fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// `client` now serves its agent. The newest offer wins: an earlier
    /// client keeps the requests it was already sent and gets no new one.
    fn offer(&mut self, client: ClientId, out_tx: mpsc::Sender<Outbound>) {
        self.offer = Some(Offer { client, out_tx });
    }

    /// Hand `message` to the offering client; its reply resolves `answer`.
    fn deliver(
        &mut self,
        message: Vec<u8>,
        answer: oneshot::Sender<Vec<u8>>,
    ) -> Result<(u64, ClientId), &'static str> {
        let offer = self.offer.as_ref().ok_or("no client offers an agent")?;
        if self.pending.len() >= MAX_PENDING {
            return Err("too many requests are waiting");
        }
        self.last_id += 1;
        let id = self.last_id;
        let event = FrameKind::Event {
            terminal: None,
            event: AgentEvent::SshAgentRequest { id, message },
            stamp: None,
        };
        offer
            .out_tx
            .try_send(Outbound::Frame(event))
            .map_err(|_| "the offering client is gone or not keeping up")?;
        let client = offer.client;
        self.pending.insert(id, Pending { client, answer });
        Ok((id, client))
    }

    /// `client` answers request `id`; false when no such request waits on
    /// that client (answered, timed out, or another client's).
    fn reply(&mut self, client: ClientId, id: u64, message: Vec<u8>) -> bool {
        match self.pending.entry(id) {
            Entry::Occupied(waiting) if waiting.get().client == client => {
                // The pane may have given up; its answer then goes nowhere.
                let _ = waiting.remove().answer.send(message);
                true
            }
            _ => false,
        }
    }

    /// `client`'s connection ended: its offer goes, and every request
    /// waiting on it fails now (dropping `answer` wakes the pane's side).
    fn withdraw(&mut self, client: ClientId) {
        if self.offer.as_ref().is_some_and(|o| o.client == client) {
            self.offer = None;
        }
        self.pending.retain(|_, waiting| waiting.client != client);
    }
}

/// `client`'s connection ended.
pub(super) fn withdraw(state: &SharedState, client: ClientId) {
    state.with_mut(|s| s.forward_agent_mut().withdraw(client));
}

/// `AGENT_OFFER` and `AGENT_REPLY`, answered on the dispatcher.
pub(super) fn admit(ctx: &CommandContext, op: phux_protocol::forward::ForwardOp) -> CommandResult {
    use phux_protocol::forward::ForwardOp;
    if ctx.state.with(|s| s.forward_agent().socket().is_none()) {
        return CommandResult::error(
            ErrorCode::Unsupported,
            "ssh-agent forwarding is turned off on this server (`[forward] agent = false`)",
        );
    }
    match op {
        ForwardOp::AgentOffer => {
            ctx.state.with_mut(|s| {
                s.forward_agent_mut()
                    .offer(ctx.client_id, ctx.out_tx.clone());
            });
            audit(
                &ctx.state,
                crate::audit::Kind::ForwardAgentOffer,
                Some(ctx.client_id),
                "AGENT_OFFER",
                None,
                Ok(()),
                None,
            );
            CommandResult::Ok
        }
        ForwardOp::AgentReply { id, message } => {
            if ctx
                .state
                .with_mut(|s| s.forward_agent_mut().reply(ctx.client_id, id, message))
            {
                CommandResult::Ok
            } else {
                CommandResult::error(
                    ErrorCode::NotFound,
                    format!("no ssh-agent request {id} is waiting on this connection"),
                )
            }
        }
        _ => CommandResult::error(ErrorCode::InternalError, "not an ssh-agent command"),
    }
}

/// Bind the broker socket and serve it until `root` is cancelled, when
/// `[forward] agent` is on. A bind failure leaves forwarding off for this
/// run: no bit, no `SSH_AUTH_SOCK`.
pub(in crate::runtime) fn start(state: &SharedState, root: &CancellationToken) {
    let (enabled, server_socket) = state.with(|s| {
        (
            s.config().forward.agent,
            s.config().server_socket_path().map(Path::to_path_buf),
        )
    });
    let Some(server_socket) = server_socket.filter(|_| enabled) else {
        return;
    };
    let path = socket_path_for(&server_socket);
    let listener = match bind(&path) {
        Ok(listener) => listener,
        Err(err) => {
            warn!(path = %path.display(), %err, "ssh-agent forwarding is off: cannot bind its socket");
            return;
        }
    };
    info!(path = %path.display(), "brokering ssh-agent requests");
    state.with_mut(|s| s.forward_agent_mut().socket = Some(path.clone()));
    let (state, root) = (state.clone(), root.clone());
    tokio::task::spawn_local(async move {
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        loop {
            let stream = tokio::select! {
                biased;
                () = root.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => stream,
                    Err(err) => {
                        warn!(%err, "ssh-agent broker accept failed");
                        continue;
                    }
                },
            };
            // Over the limit the connection is dropped: ssh reads the end.
            let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
                continue;
            };
            let state = state.clone();
            tokio::task::spawn_local(async move {
                serve(&state, stream, REQUEST_DEADLINE).await;
                drop(slot);
            });
        }
        let _ = std::fs::remove_file(&path);
    });
}

/// Bind `path` owner-only. Whatever sits there is a previous run's: this
/// process holds the server socket the name derives from.
fn bind(path: &Path) -> Result<UnixListener, crate::ServerError> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
        _ => {}
    }
    let listener = UnixListener::bind(path)?;
    crate::runtime::connections::socket::secure_socket_file(path)?;
    Ok(listener)
}

/// Serve one ssh-agent connection: length-prefixed requests, one answer
/// each, in order. A zero or over-long length ends the connection, since
/// the stream cannot be resynchronized.
pub(super) async fn serve(state: &SharedState, mut stream: UnixStream, deadline: Duration) {
    loop {
        let Ok(len) = stream.read_u32().await else {
            return;
        };
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        let reply = if len == 0 || len > MAX_AGENT_MESSAGE_BYTES {
            None
        } else {
            let mut message = vec![0; len];
            if stream.read_exact(&mut message).await.is_err() {
                return;
            }
            Some(ask(state, message, deadline).await)
        };
        let body = reply.as_deref().unwrap_or(&[SSH_AGENT_FAILURE]);
        let mut framed = u32::try_from(body.len())
            .unwrap_or(1)
            .to_be_bytes()
            .to_vec();
        framed.extend_from_slice(body);
        if stream.write_all(&framed).await.is_err() || reply.is_none() {
            return;
        }
    }
}

/// Forgets a request however its wait ends.
struct Unwait<'a>(&'a SharedState, u64);

impl Drop for Unwait<'_> {
    fn drop(&mut self) {
        self.0.with_mut(|s| {
            s.forward_agent_mut().pending.remove(&self.1);
        });
    }
}

/// The answer to one pane request: the offering client's, when the
/// request is a list or a sign and the answer is of the matching type
/// within `deadline`; `SSH_AGENT_FAILURE` otherwise.
async fn ask(state: &SharedState, message: Vec<u8>, deadline: Duration) -> Vec<u8> {
    let failure = || vec![SSH_AGENT_FAILURE];
    // Add, remove, lock, and every other request stop here.
    let Some(expected) = agent_answer_type(&message) else {
        return failure();
    };
    // Only a signature is recorded; a listing reveals no more than
    // `ssh-add -L` does.
    let key = sign_request_key(&message).map(fingerprint);
    let (answer, answered) = oneshot::channel();
    let delivered = state.with_mut(|s| s.forward_agent_mut().deliver(message, answer));
    let (id, client) = match delivered {
        Ok(delivered) => delivered,
        Err(reason) => {
            audit_sign(state, None, key, Err(reason), None);
            return failure();
        }
    };
    let unwait = Unwait(state, id);
    let (reply, result) = match tokio::time::timeout(deadline, answered).await {
        Ok(Ok(reply)) if reply.first() == Some(&expected) => (reply, Ok(())),
        Ok(Ok(_)) => (failure(), Err("the offering client refused")),
        Ok(Err(_)) => (failure(), Err("the offering client disconnected")),
        Err(_) => (failure(), Err("the offering client did not answer in time")),
    };
    drop(unwait);
    audit_sign(state, Some(client), key, result, Some(id));
    reply
}

/// A key's fingerprint as `ssh-add -l` prints it.
fn fingerprint(key: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(key);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// `forward.agent.sign` for a sign request; nothing for a listing.
fn audit_sign(
    state: &SharedState,
    client: Option<ClientId>,
    key: Option<String>,
    result: Result<(), &'static str>,
    request: Option<u64>,
) {
    if key.is_some() {
        let kind = crate::audit::Kind::ForwardAgentSign;
        audit(state, kind, client, "AGENT_SIGN", key, result, request);
    }
}

/// One audit record: `client` is the offering client (the server itself
/// when there is none), `target` the key's fingerprint.
fn audit(
    state: &SharedState,
    kind: crate::audit::Kind,
    client: Option<ClientId>,
    action: &str,
    target: Option<String>,
    result: Result<(), &'static str>,
    request: Option<u64>,
) {
    if !crate::audit::enabled() {
        return;
    }
    let actor = client.map_or_else(crate::audit::AuditActor::server, |client| {
        state.with(|s| s.audit_actor(client))
    });
    let outcome = match result {
        Ok(()) => crate::audit::Outcome::Allowed,
        Err(_) => crate::audit::Outcome::Denied,
    };
    crate::audit::emit(
        kind,
        actor,
        action,
        target,
        outcome,
        serde_json::json!({
            "client": client.map(|client| client.0),
            "request": request,
            "reason": result.err(),
        }),
    );
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
