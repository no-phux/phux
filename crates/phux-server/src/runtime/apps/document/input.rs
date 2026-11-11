//! `SEND_DOCUMENT_INPUT`: relay a viewer's typed intent to the connection
//! producing a document. The server reads none of it; the producer maps it
//! to its source (`docs/resource-kinds.md#producers`). The exceptions are
//! the clipboard: a copy is held open until the producer answers with the
//! text, so only the viewer that asked receives it, and a paste is held
//! briefly for the producer's outcome, so a paste that could not land says
//! so.

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
/// How long a paste waits for the producer's outcome. A producer that
/// predates the answer never sends one, so silence is success.
const PASTE_TIMEOUT: Duration = Duration::from_secs(3);

/// A copy or paste waiting on a producer: its document, the producer that
/// must answer, and where the answer goes. A paste's answer carries reply
/// 0, since a paste names no reply on the wire; the oldest paste waiting
/// on that document and producer takes it.
struct Waiting {
    document: phux_core::ids::ResourceId,
    producer: ClientId,
    paste: bool,
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
    let paste = matches!(intent, DocumentInput::Paste { .. });
    let (intent, held) = if paste || matches!(intent, DocumentInput::CopyClipboard { .. }) {
        let reply = NEXT_REPLY.fetch_add(1, Ordering::Relaxed);
        let (answer, rx) = oneshot::channel();
        waiting().insert(
            reply,
            Waiting {
                document: core,
                producer: spawner,
                paste,
                answer,
            },
        );
        let intent = if paste {
            intent
        } else {
            DocumentInput::CopyClipboard { reply }
        };
        (intent, Some((Unwait(reply), rx)))
    } else {
        (intent, None)
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
    let Some((_unwait, rx)) = held else {
        return CommandResult::Ok;
    };
    settle(paste, A::LABEL, rx).await
}

/// The viewer's result for a held copy or paste, once the producer answers
/// or the wait runs out.
async fn settle(
    paste: bool,
    label: &str,
    rx: oneshot::Receiver<Result<String, String>>,
) -> CommandResult {
    let unavailable = |message: String| CommandResult::Error {
        code: ErrorCode::Unavailable,
        message,
    };
    if paste {
        return match tokio::time::timeout(PASTE_TIMEOUT, rx).await {
            Ok(Ok(Err(message))) => unavailable(message),
            _ => CommandResult::Ok,
        };
    }
    match tokio::time::timeout(COPY_TIMEOUT, rx).await {
        Ok(Ok(Ok(text))) => CommandResult::OkWith(CommandValue::Bytes(text.into_bytes())),
        Ok(Ok(Err(message))) => unavailable(message),
        Ok(Err(_)) | Err(_) => {
            unavailable(format!("the {label}'s producer did not answer the copy"))
        }
    }
}

/// The producer `client` answers copy `reply` (or, with 0, its oldest
/// paste) on `document`. Anyone else, or a reply nothing waits on, is
/// refused and the copy keeps waiting.
fn answer(
    document: phux_core::ids::ResourceId,
    client: ClientId,
    reply: u32,
    text: Result<String, String>,
) -> CommandResult {
    let mut table = waiting();
    let ours = |w: &Waiting| w.document == document && w.producer == client;
    // Reply 0 answers this producer's oldest paste on the document.
    let reply = if reply == 0 {
        table
            .iter()
            .filter(|(_, w)| w.paste && ours(w))
            .map(|(reply, _)| *reply)
            .min()
            .unwrap_or(0)
    } else {
        reply
    };
    let entry = match table.entry(reply) {
        Entry::Occupied(waiting) if ours(waiting.get()) => Some(waiting.remove()),
        _ => None,
    };
    drop(table);
    let Some(entry) = entry else {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "no copy or paste from this producer is waiting on that reply".to_owned(),
        };
    };
    // The viewer may have given up; its answer then goes nowhere.
    let _ = entry.answer.send(text);
    CommandResult::Ok
}
