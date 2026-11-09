//! `SEND_DOCUMENT_INPUT`: relay a viewer's typed intent to the connection
//! producing a document. The server reads none of it; the producer maps it
//! to its source (`docs/resource-kinds.md#producers`). The one exception is
//! a clipboard copy, which the server holds open until the producer answers
//! with the text, so only the viewer that asked receives it.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use phux_protocol::wire::frame::{
    AgentEvent, CommandResult, CommandValue, ControlClasses, DocumentInput, ErrorCode, FrameKind,
};
use phux_tree::DocumentResource as _;
use tokio::sync::oneshot;

use super::DocumentApi;
use crate::mailbox::Outbound;
use crate::runtime::commands::CommandContext;
use crate::state::ClientId;

/// How long a copy waits for the producer to read the host clipboard.
const COPY_TIMEOUT: Duration = Duration::from_secs(10);

/// A copy waiting on a producer: its document, the producer that must
/// answer, and where the answer goes.
struct Waiting {
    document: phux_core::ids::ResourceId,
    producer: ClientId,
    answer: oneshot::Sender<Result<String, String>>,
}

// ponytail: one process-wide table keyed by a process-wide counter, so two
// servers in one test process never share a token; move it into
// `ServerState` if it ever needs more than insert-and-take.
static NEXT_REPLY: AtomicU32 = AtomicU32::new(1);
static WAITING: LazyLock<Mutex<HashMap<u32, Waiting>>> = LazyLock::new(Mutex::default);

fn waiting() -> std::sync::MutexGuard<'static, HashMap<u32, Waiting>> {
    WAITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Removes a copy's table entry however its wait ends.
struct Unwait(u32);

impl Drop for Unwait {
    fn drop(&mut self) {
        waiting().remove(&self.0);
    }
}

/// Queue `intent` on the producer's connection as an unjournaled
/// `document_input` event. The producer is the connection that spawned the
/// document; once it is gone the intent is refused, never held. `control`,
/// stamped by a relaying hub, refuses a document of any other class.
/// An `input_activity` naming the sender goes first, so the producer can
/// show who is in control.
pub(super) async fn relay<A: DocumentApi>(
    ctx: &CommandContext,
    terminal_id: &phux_protocol::ids::ResourceId,
    intent: DocumentInput,
    control: Option<ControlClasses>,
) -> CommandResult {
    let found = ctx.state.with(|s| {
        let core = s.resource_from_wire(terminal_id)?;
        let handle = s.resource_handle(core)?.clone();
        let producer = s.resource_producer(core);
        let spawner = s.resource_spawner(core);
        let origin = s.input_origin(ctx.client_id);
        Some((
            core,
            handle,
            producer,
            spawner,
            s.document_control(core),
            origin,
        ))
    });
    let Some((core, handle, producer, spawner, class, origin)) = found else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such resource: {terminal_id}"),
        };
    };
    if let Err(error) = handle.document::<A>() {
        return crate::runtime::commands::wrong_resource_kind(error);
    }
    if let DocumentInput::ClipboardText { reply, text } = intent {
        return answer(core, ctx.client_id, reply, text);
    }
    if let Some(allowed) = control
        && !class.is_some_and(|class| allowed.contains(class))
    {
        return CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message: format!("input to this {} was not authorized", A::LABEL),
        };
    }
    let not_connected = || CommandResult::Error {
        code: ErrorCode::NotProducer,
        message: format!("the {}'s producer is not connected", A::LABEL),
    };
    let (Some(producer), Some(spawner)) = (producer, spawner) else {
        return not_connected();
    };
    let (intent, copy) = match intent {
        DocumentInput::CopyClipboard { .. } => {
            let reply = NEXT_REPLY.fetch_add(1, Ordering::Relaxed);
            let (answer, rx) = oneshot::channel();
            waiting().insert(
                reply,
                Waiting {
                    document: core,
                    producer: spawner,
                    answer,
                },
            );
            (
                DocumentInput::CopyClipboard { reply },
                Some((Unwait(reply), rx)),
            )
        }
        intent => (intent, None),
    };
    // Best effort: a full mailbox refuses the intent below anyway.
    let _ = producer.try_send(Outbound::Frame(FrameKind::Event {
        terminal: Some(terminal_id.clone()),
        event: AgentEvent::InputActivity { origin },
        stamp: None,
    }));
    let event = FrameKind::Event {
        terminal: Some(terminal_id.clone()),
        event: AgentEvent::DocumentInput { intent },
        stamp: None,
    };
    match producer.try_send(Outbound::Frame(event)) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            return CommandResult::Error {
                code: ErrorCode::Overflow,
                message: format!("the {}'s producer is not keeping up", A::LABEL),
            };
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return not_connected(),
    }
    let Some((_unwait, rx)) = copy else {
        return CommandResult::Ok;
    };
    let unavailable = |message: String| CommandResult::Error {
        code: ErrorCode::Unavailable,
        message,
    };
    match tokio::time::timeout(COPY_TIMEOUT, rx).await {
        Ok(Ok(Ok(text))) => CommandResult::OkWith(CommandValue::Bytes(text.into_bytes())),
        Ok(Ok(Err(message))) => unavailable(message),
        Ok(Err(_)) | Err(_) => unavailable(format!(
            "the {}'s producer did not answer the copy",
            A::LABEL
        )),
    }
}

/// The producer `client` answers copy `reply` on `document`. Anyone else,
/// or a reply nothing waits on, is refused and the copy keeps waiting.
fn answer(
    document: phux_core::ids::ResourceId,
    client: ClientId,
    reply: u32,
    text: Result<String, String>,
) -> CommandResult {
    let entry = match waiting().entry(reply) {
        Entry::Occupied(waiting)
            if waiting.get().document == document && waiting.get().producer == client =>
        {
            Some(waiting.remove())
        }
        _ => None,
    };
    let Some(entry) = entry else {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "no copy from this producer is waiting on that reply".to_owned(),
        };
    };
    // The viewer may have given up; its answer then goes nowhere.
    let _ = entry.answer.send(text);
    CommandResult::Ok
}
