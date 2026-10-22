//! The event journal over the wire (ADR-0123, `docs/spec/L1.md` §7.3): one
//! server-wide `seq`, cursor replay, typed gaps for every loss, one registry
//! behind both subscribe verbs, and the actor behind each event or metadata
//! change.
//!
//! Events are driven with `REPORT_ASKED`, which queues its `asked` event
//! before replying, so "events before the reply" is exact. `SUBSCRIBE_EVENTS`
//! has no reply; a following `GET_STATE` is the barrier, and a cursor replay
//! is sent before that barrier's reply.

mod attribution;
mod replay;

use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, EventStamp, FrameKind, SpawnResult,
    StateScope,
};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::{Instant, timeout};

use phux_server_testkit::{
    ServerHandles, Spawn, WIRE_RECV_TIMEOUT, join_after_shutdown, recv_typed, recv_typed_before,
    send_frame, spawn_server_with,
};

use crate::common::{attach_pane, connect_as, full_caps};

/// One observed `EVENT` frame.
#[derive(Debug, Clone)]
struct Seen {
    terminal: Option<ResourceId>,
    event: AgentEvent,
    stamp: Option<Box<EventStamp>>,
}

impl Seen {
    fn seq(&self) -> u64 {
        self.stamp
            .as_ref()
            .expect("a journaled event is stamped")
            .seq
    }

    fn asked_id(&self) -> Option<&str> {
        match &self.event {
            AgentEvent::Asked { id, .. } => Some(id),
            _ => None,
        }
    }

    fn actor_name(&self) -> Option<&str> {
        self.stamp.as_ref()?.actor.as_ref()?.client_name.as_deref()
    }

    const fn is_gap(&self) -> bool {
        matches!(self.event, AgentEvent::JournalGap { .. })
    }
}

fn as_seen(frame: FrameKind) -> Option<Seen> {
    match frame {
        FrameKind::Event {
            terminal,
            event,
            stamp,
        } => Some(Seen {
            terminal,
            event,
            stamp,
        }),
        _ => None,
    }
}

/// A server seeding session `demo` without a PTY, with an optional journal size.
struct Server {
    tmp: TempDir,
    handles: ServerHandles,
}

impl Server {
    fn start(journal_entries: Option<u32>) -> Self {
        let tmp = TempDir::new().unwrap();
        let handles = spawn_server_with(tmp.path().join("phux.sock"), Some("demo"), |cfg| {
            if let Some(entries) = journal_entries {
                cfg.event_journal_entries = entries;
            }
        });
        Self { tmp, handles }
    }

    fn socket(&self) -> std::path::PathBuf {
        self.tmp.path().join("phux.sock")
    }

    async fn connect(&self, name: &str) -> UnixStream {
        connect_as(&self.socket(), name, full_caps()).await.0
    }

    /// A connection named `name`, attached to `demo`, and its pane.
    async fn producer(&self, name: &str) -> (UnixStream, ResourceId) {
        let mut stream = self.connect(name).await;
        let pane = attach_pane(&mut stream, "demo").await;
        (stream, pane)
    }

    async fn stop(self) {
        let (shutdown, server) = self.handles;
        join_after_shutdown(shutdown, server).await;
    }
}

/// Every `EVENT` that arrives before the reply to `request_id`.
async fn command(
    stream: &mut UnixStream,
    request_id: u32,
    command: Command,
) -> (CommandResult, Vec<Seen>) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    let deadline = Instant::now() + WIRE_RECV_TIMEOUT;
    let mut seen = Vec::new();
    loop {
        let (_, frame) = recv_typed_before(stream, deadline)
            .await
            .unwrap_or_else(|| {
                panic!("no reply to request {request_id} within {WIRE_RECV_TIMEOUT:?}")
            });
        match frame {
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => return (result, seen),
            other => seen.extend(as_seen(other)),
        }
    }
}

const fn get_state() -> Command {
    Command::GetState {
        scope: StateScope::Server,
    }
}

/// `SUBSCRIBE_EVENTS` then the barrier; returns the replay sent ahead of it.
async fn subscribe(
    stream: &mut UnixStream,
    request_id: u32,
    terminal: Option<ResourceId>,
    after_seq: Option<u64>,
) -> Vec<Seen> {
    send_frame(
        stream,
        &FrameKind::SubscribeEvents {
            terminal,
            after_seq,
        },
    )
    .await;
    let (result, seen) = command(stream, request_id, get_state()).await;
    assert!(
        !matches!(result, CommandResult::Error { .. }),
        "barrier: {result:?}"
    );
    seen
}

fn report_asked(terminal: &ResourceId, id: &str) -> Command {
    Command::ReportAsked {
        terminal_id: terminal.clone(),
        id: id.to_owned(),
        question: format!("question {id}?"),
        suggestions: Vec::new(),
        elapsed_seconds: None,
    }
}

/// `REPORT_ASKED` for its event; the reply must be `Ok`.
async fn ask(stream: &mut UnixStream, request_id: u32, terminal: &ResourceId, id: &str) {
    let (result, _) = command(stream, request_id, report_asked(terminal, id)).await;
    assert_eq!(result, CommandResult::Ok, "REPORT_ASKED {id}");
}

/// `REPORT_ASKED` with a large question, to fill buffers quickly.
async fn ask_padded(
    stream: &mut UnixStream,
    request_id: u32,
    terminal: &ResourceId,
    id: String,
    pad: &str,
) {
    let report = Command::ReportAsked {
        terminal_id: terminal.clone(),
        id,
        question: pad.to_owned(),
        suggestions: Vec::new(),
        elapsed_seconds: None,
    };
    let (result, _) = command(stream, request_id, report).await;
    assert_eq!(result, CommandResult::Ok, "the producer is never blocked");
}

/// The next `EVENT` `matches` accepts.
async fn next_event(stream: &mut UnixStream, matches: impl Fn(&Seen) -> bool) -> Seen {
    let deadline = Instant::now() + WIRE_RECV_TIMEOUT;
    loop {
        let (_, frame) = recv_typed_before(stream, deadline)
            .await
            .unwrap_or_else(|| panic!("no matching EVENT within {WIRE_RECV_TIMEOUT:?}"));
        if let Some(seen) = as_seen(frame)
            && matches(&seen)
        {
            return seen;
        }
    }
}

/// Every event up to and including the first `done` accepts.
async fn read_until(stream: &mut UnixStream, done: impl Fn(&Seen) -> bool) -> Vec<Seen> {
    let mut seen = Vec::new();
    loop {
        let frame = next_event(stream, |_| true).await;
        let finished = done(&frame);
        seen.push(frame);
        if finished {
            return seen;
        }
    }
}

/// Every `EVENT` until the stream has been quiet for `quiet`.
async fn drain_until_quiet(stream: &mut UnixStream, quiet: Duration) -> Vec<Seen> {
    let mut seen = Vec::new();
    while let Ok((_, frame)) = timeout(quiet, recv_typed(stream)).await {
        seen.extend(as_seen(frame));
    }
    seen
}

fn assert_strictly_increasing(seen: &[Seen]) {
    let seqs: Vec<u64> = seen
        .iter()
        .filter(|s| s.stamp.is_some())
        .map(Seen::seq)
        .collect();
    for pair in seqs.windows(2) {
        assert!(pair[0] < pair[1], "seq went {} then {}", pair[0], pair[1]);
    }
}

fn asked_ids(seen: &[Seen]) -> Vec<&str> {
    seen.iter().filter_map(Seen::asked_id).collect()
}

/// Spawn `/bin/sh -c script`; returns the id and every `EVENT` ahead of the reply.
async fn spawn_pane(
    stream: &mut UnixStream,
    request_id: u32,
    script: &str,
) -> (ResourceId, Vec<Seen>) {
    send_frame(
        stream,
        &Spawn::command(&["/bin/sh", "-c", script]).frame(request_id),
    )
    .await;
    let deadline = Instant::now() + WIRE_RECV_TIMEOUT;
    let mut before = Vec::new();
    loop {
        let (_, frame) = recv_typed_before(stream, deadline)
            .await
            .unwrap_or_else(|| {
                panic!("no RESOURCE_SPAWNED for request {request_id} within {WIRE_RECV_TIMEOUT:?}")
            });
        match frame {
            FrameKind::ResourceSpawned {
                request_id: got,
                result,
            } if got == request_id => {
                let SpawnResult::Ok(id) = result else {
                    panic!("spawn failed: {result:?}");
                };
                return (id, before);
            }
            other => before.extend(as_seen(other)),
        }
    }
}

fn journal_head(result: CommandResult) -> u64 {
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("GET_STATE failed: {result:?}");
    };
    snapshot
        .journal_head()
        .expect("an EVENT_JOURNAL server writes the head")
}

/// Every event the journal still holds, via a fresh connection's cursor replay.
async fn journal_contents(socket: &Path) -> Vec<Seen> {
    let (mut reader, _) = connect_as(socket, "journal-reader", full_caps()).await;
    send_frame(
        &mut reader,
        &FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: Some(0),
        },
    )
    .await;
    drain_until_quiet(&mut reader, Duration::from_millis(400)).await
}
