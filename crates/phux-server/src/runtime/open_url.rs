//! Opening a pane's link on the machine of the person typing into it
//! (`OPEN_URL`, `OPEN_URL_REPLY`, behind `features_ext.OPEN_URL`;
//! `docs/ssh.md#opening-links`).
//!
//! One client hears of a request: the attached one that last typed into the
//! pane, if it can write there and offered `OPEN_URL` at `HELLO`. The table
//! lives in `ServerState`, so each step is one lock.

#![expect(
    clippy::redundant_pub_crate,
    reason = "the server's state reaches this table"
)]

use std::collections::HashMap;
use std::time::Duration;

use phux_config::open_url::WebLink;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::open_url::{OPEN_URL_SECS, OpenUrlOp};
use phux_protocol::wire::frame::{AgentEvent, CommandResult, ErrorCode, FrameKind};
use tokio::sync::{mpsc, oneshot};

use super::commands::CommandContext;
use crate::audit::Outcome;
use crate::state::{ClientId, Outbound, ServerState, SharedState};

/// Requests waiting on an answer at once, one per pane; one more is refused.
const MAX_PENDING: usize = 8;
const DEADLINE: Duration = Duration::from_secs(OPEN_URL_SECS);

/// One request waiting on `client`'s answer.
#[derive(Debug)]
struct Pending {
    terminal: WireResourceId,
    client: ClientId,
    answer: oneshot::Sender<bool>,
}

/// The server's waiting requests, by id.
#[derive(Debug, Default)]
pub(crate) struct OpenUrls {
    pending: HashMap<u64, Pending>,
    /// Never reused within a server process.
    last_id: u64,
}

impl OpenUrls {
    /// `client` left its session: what waits on it is declined now.
    pub(crate) fn withdraw(&mut self, client: ClientId) {
        self.pending.retain(|_, waiting| waiting.client != client);
    }
}

pub(super) async fn handle(
    ctx: &CommandContext,
    terminal: WireResourceId,
    op: OpenUrlOp,
) -> CommandResult {
    match op {
        OpenUrlOp::Request { url } => request(ctx, terminal, &url, DEADLINE).await,
        OpenUrlOp::Reply { id, opened } => reply(ctx, &terminal, id, opened),
    }
}

/// Forgets a request however its wait ends, the asker's disconnect included.
struct Unwait<'a>(&'a SharedState, u64);

impl Drop for Unwait<'_> {
    fn drop(&mut self) {
        self.0.with_mut(|s| {
            s.open_urls_mut().pending.remove(&self.1);
        });
    }
}

async fn request(
    ctx: &CommandContext,
    terminal: WireResourceId,
    raw: &str,
    deadline: Duration,
) -> CommandResult {
    let link = match WebLink::parse(raw) {
        Ok(link) => link,
        Err(why) => return CommandResult::error(ErrorCode::InvalidCommand, why),
    };
    let record = |client, id: Option<u64>, outcome, reason| {
        audit(&ctx.state, client, &terminal, id, &link, outcome, reason);
    };
    let (answer, answered) = oneshot::channel();
    let asked = ctx
        .state
        .with_mut(|s| deliver(s, &terminal, link.as_str(), answer));
    let (id, client) = match asked {
        Ok(asked) => asked,
        Err((code, why)) => {
            record(ctx.client_id, None, Outcome::Denied, Some(why));
            return CommandResult::error(code, why);
        }
    };
    let _unwait = Unwait(&ctx.state, id);
    record(ctx.client_id, Some(id), Outcome::Allowed, None);
    let refusal = match tokio::time::timeout(deadline, answered).await {
        Ok(Ok(true)) => None,
        Ok(Ok(false)) => Some("the person declined"),
        Ok(Err(_)) => Some("the person's client left"),
        Err(_) => Some("the person did not answer in time"),
    };
    let outcome = refusal.map_or(Outcome::Approved, |_| Outcome::Rejected);
    record(client, Some(id), outcome, refusal);
    refusal.map_or(CommandResult::Ok, |why| {
        CommandResult::error(ErrorCode::Unavailable, why)
    })
}

/// The client a request from `terminal` goes to: the one that last typed
/// into it, while it is attached to it, may write to it and prompts for links.
fn asks(s: &ServerState, terminal: &WireResourceId) -> Option<(ClientId, mpsc::Sender<Outbound>)> {
    let client = s.clients().controller(terminal)?;
    let core = s.idspace.resource_from_wire(terminal)?;
    let attached = s.attached().get(&client)?;
    let asks = attached.client_caps.open_url
        && s.resources().subscribers_for(core).contains(&client)
        && !s.clients().is_viewer(client, terminal);
    asks.then(|| (client, attached.tx.clone()))
}

/// Queue the request on its client; `answer` hears the reply.
fn deliver(
    s: &mut ServerState,
    terminal: &WireResourceId,
    url: &str,
    answer: oneshot::Sender<bool>,
) -> Result<(u64, ClientId), (ErrorCode, &'static str)> {
    const NOBODY: (ErrorCode, &str) = (
        ErrorCode::ClientNotFound,
        "no attached client that opens links is typing in this pane",
    );
    let (client, tx) = asks(s, terminal).ok_or(NOBODY)?;
    let urls = s.open_urls_mut();
    if urls
        .pending
        .values()
        .any(|waiting| waiting.terminal == *terminal)
    {
        let why = "this pane already has a link waiting";
        return Err((ErrorCode::ResourceExhausted, why));
    }
    if urls.pending.len() >= MAX_PENDING {
        return Err((ErrorCode::ResourceExhausted, "too many links are waiting"));
    }
    urls.last_id += 1;
    let id = urls.last_id;
    let event = FrameKind::Event {
        terminal: Some(terminal.clone()),
        event: AgentEvent::OpenUrl {
            id,
            url: url.to_owned(),
        },
        stamp: None,
    };
    tx.try_send(Outbound::Frame(event)).map_err(|_| NOBODY)?;
    let terminal = terminal.clone();
    urls.pending.insert(
        id,
        Pending {
            terminal,
            client,
            answer,
        },
    );
    Ok((id, client))
}

/// Only the client that was asked answers, for the pane that asked.
fn reply(ctx: &CommandContext, terminal: &WireResourceId, id: u64, opened: bool) -> CommandResult {
    let waiting = ctx.state.with_mut(|s| {
        let pending = &mut s.open_urls_mut().pending;
        let waits = pending.get(&id).is_some_and(|waiting| {
            waiting.client == ctx.client_id && waiting.terminal == *terminal
        });
        waits.then(|| pending.remove(&id)).flatten()
    });
    let Some(waiting) = waiting else {
        return CommandResult::error(
            ErrorCode::NotFound,
            format!("no link request {id} is waiting on this connection"),
        );
    };
    let _ = waiting.answer.send(opened);
    CommandResult::Ok
}

/// One `open_url` record: the request (the actor asked), then its outcome
/// (the actor is the client that was asked). The link's host only, since a
/// query can hold a secret.
fn audit(
    state: &SharedState,
    client: ClientId,
    terminal: &WireResourceId,
    id: Option<u64>,
    link: &WebLink,
    outcome: Outcome,
    reason: Option<&str>,
) {
    if !crate::audit::enabled() {
        return;
    }
    let answered = matches!(outcome, Outcome::Approved | Outcome::Rejected);
    crate::audit::emit(
        crate::audit::Kind::OpenUrl,
        state.with(|s| s.audit_actor(client)),
        if answered {
            "OPEN_URL_REPLY"
        } else {
            "OPEN_URL"
        },
        Some(crate::audit::terminal_selector(terminal)),
        outcome,
        serde_json::json!({
            "client": client.0,
            "request": id,
            "host": link.host(),
            "reason": reason,
        }),
    );
}

#[cfg(test)]
#[path = "open_url_tests.rs"]
mod tests;
