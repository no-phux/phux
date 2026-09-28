//! The event journal over the wire (ADR-0123, `docs/spec/L1.md` §7.3): one
//! server-wide `seq`, cursor replay, typed gaps for every loss, one registry
//! behind both subscribe verbs, and the actor behind each event or metadata
//! change.
//!
//! Events are driven with `REPORT_ASKED`, which queues its `asked` event
//! before replying, so "events before the reply" is exact. `SUBSCRIBE_EVENTS`
//! has no reply; a following `GET_STATE` is the barrier, and a cursor replay
//! is sent before that barrier's reply.

use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, EventStamp, FrameKind,
    InputMode, ResourceEventType, SESSION_CREATE_KEY, SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY,
    Scope, SpawnResult, StateScope,
};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    ServerHandles, Spawn, WIRE_RECV_TIMEOUT, join_after_shutdown, recv_typed, recv_until,
    run_local, send_frame, spawn_server_with, spawn_server_with_seed_cmd,
};

use crate::common::{attach_pane, connect_as, full_caps, gated_seed};

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
    let mut seen = Vec::new();
    loop {
        let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the reply arrives within the deadline");
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
    loop {
        let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the event arrives within the deadline");
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
    let mut before = Vec::new();
    loop {
        let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the spawn completes within the deadline");
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

/// Stamps carry one server-wide `seq` across resources, Unix-ms `ts`, and the
/// HELLO `client_name` of the connection that caused the event.
#[test]
fn events_carry_monotone_seq_ts_and_the_hello_name_as_actor() {
    run_local(async {
        let server = Server::start(None);
        let (mut a, first) = server.producer("orchestrator").await;
        let _ = subscribe(&mut a, 1, None, None).await;

        let (second, before) = spawn_pane(&mut a, 2, "sleep 600").await;
        let announces = |s: &Seen| {
            s.terminal.as_ref() == Some(&second)
                && matches!(s.event, AgentEvent::ResourceSpawned { .. })
        };
        let spawned = match before.into_iter().find(|s| announces(s)) {
            Some(seen) => seen,
            None => next_event(&mut a, announces).await,
        };
        let (_, one) = command(&mut a, 3, report_asked(&first, "on-first")).await;
        let (_, two) = command(&mut a, 4, report_asked(&second, "on-second")).await;
        let asked: Vec<Seen> = one
            .into_iter()
            .chain(two)
            .filter(|s| s.asked_id().is_some())
            .collect();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(asked[0].terminal.as_ref(), Some(&first));
        assert_eq!(asked[1].terminal.as_ref(), Some(&second));
        let ordered = [&spawned, &asked[0], &asked[1]];
        for pair in ordered.windows(2) {
            assert!(
                pair[0].seq() < pair[1].seq(),
                "one order across resources: {pair:?}"
            );
        }
        for seen in ordered {
            assert!(seen.stamp.as_ref().unwrap().ts_ms > 1_600_000_000_000);
        }
        assert_eq!(spawned.actor_name(), Some("orchestrator"));

        // An unsubscribed spawner is still named on a watcher's stream.
        let (mut labeler, _) = server.producer("labeler").await;
        let (labeled, _) = spawn_pane(&mut labeler, 2, "sleep 600").await;
        let seen = next_event(&mut a, |s| {
            s.terminal.as_ref() == Some(&labeled)
                && matches!(s.event, AgentEvent::ResourceSpawned { .. })
        })
        .await;
        let actor = seen
            .stamp
            .as_ref()
            .unwrap()
            .actor
            .as_ref()
            .expect("attributed");
        assert_eq!(actor.client_name.as_deref(), Some("labeler"));
        assert_eq!(
            actor.credential_id, None,
            "a local socket carries no credential"
        );

        drop((a, labeler));
        server.stop().await;
    });
}

/// A cursor replays missed events in order then goes live; overlapping scopes
/// (server-wide, per-terminal, `SUBSCRIBE_RESOURCE_EVENTS`) on one connection
/// deliver each event once.
#[test]
fn cursor_replay_goes_live_and_overlapping_scopes_deliver_once() {
    run_local(async {
        let server = Server::start(None);
        let (mut a, pane) = server.producer("producer").await;
        for (n, id) in ["q1", "q2", "q3"].into_iter().enumerate() {
            ask(&mut a, 10 + u32::try_from(n).unwrap(), &pane, id).await;
        }

        let mut b = server.connect("late").await;
        let mut seen = subscribe(&mut b, 1, None, Some(0)).await;
        assert!(
            seen.iter().all(|s| !s.is_gap()),
            "the journal holds everything: {seen:?}"
        );
        assert_eq!(asked_ids(&seen), ["q1", "q2", "q3"], "replayed in order");
        seen.extend(subscribe(&mut b, 2, Some(pane.clone()), Some(0)).await);
        let subscribe_resource = Command::SubscribeResourceEvents {
            terminal_id: pane.clone(),
            event_types: Vec::new(),
        };
        assert_eq!(
            command(&mut b, 3, subscribe_resource).await.0,
            CommandResult::Ok
        );
        ask(&mut a, 20, &pane, "q4").await;
        seen.extend(read_until(&mut b, |s| s.asked_id() == Some("q4")).await);
        assert_eq!(
            asked_ids(&seen),
            ["q1", "q2", "q3", "q4"],
            "each event once"
        );
        assert_strictly_increasing(&seen);

        drop((a, b));
        server.stop().await;
    });
}

/// `GET_STATE` names the journal head at its cut, per connection: the newest
/// `seq` its subscriptions admit, or the newest assigned without any.
#[test]
fn the_journal_head_is_the_newest_seq_the_connection_admits() {
    run_local(async {
        let server = Server::start(None);
        let (mut a, pane) = server.producer("producer").await;
        let mut everything = subscribe(&mut a, 1, None, None).await;
        let mut b = server.connect("watcher").await;
        let mut watched = subscribe(&mut b, 1, Some(pane.clone()), None).await;
        let (_, asked) = command(&mut a, 2, report_asked(&pane, "q1")).await;
        everything.extend(asked);
        watched.push(next_event(&mut b, |s| s.asked_id() == Some("q1")).await);

        let (result, before_cut) = command(&mut a, 3, get_state()).await;
        everything.extend(before_cut);
        let newest = everything
            .iter()
            .filter(|s| s.stamp.is_some())
            .map(Seen::seq)
            .max();
        assert_eq!(Some(journal_head(result)), newest, "server-wide subscriber");

        let (other, _) = spawn_pane(&mut a, 4, "sleep 600").await;
        ask(&mut a, 5, &other, "q2").await;
        let (result, before_cut) = command(&mut b, 2, get_state()).await;
        watched.extend(before_cut);
        let watcher_head = journal_head(result);
        let mut c = server.connect("bystander").await;
        let global_head = journal_head(command(&mut c, 1, get_state()).await.0);
        let q2 = journal_contents(&server.socket())
            .await
            .iter()
            .find(|s| s.asked_id() == Some("q2"))
            .map(Seen::seq)
            .expect("q2 was journaled");
        let newest_watched = watched
            .iter()
            .filter(|s| s.stamp.is_some())
            .map(Seen::seq)
            .max();
        assert_eq!(
            Some(watcher_head),
            newest_watched,
            "per-terminal subscriber"
        );
        assert!(
            watcher_head < q2,
            "another terminal's event does not raise it"
        );
        assert!(
            global_head >= q2,
            "no subscription: the newest seq assigned"
        );

        drop((a, b, c));
        server.stop().await;
    });
}

/// A cursor older than the ring gets one unstamped gap first (before any later
/// event exists), then the retained tail.
#[test]
fn subscribe_with_a_stale_cursor_receives_journal_gap_first() {
    run_local(async {
        let server = Server::start(Some(2));
        let (mut a, pane) = server.producer("producer").await;
        for (n, id) in ["q1", "q2", "q3", "q4"].into_iter().enumerate() {
            ask(&mut a, 10 + u32::try_from(n).unwrap(), &pane, id).await;
        }
        let mut b = server.connect("stale").await;
        let replay = subscribe(&mut b, 1, None, Some(0)).await;
        let Some(AgentEvent::JournalGap {
            first_missing,
            last_missing,
        }) = replay.first().map(|s| s.event.clone())
        else {
            panic!("the first frame is the gap: {replay:?}");
        };
        assert_eq!(first_missing, 1);
        assert!(replay[0].stamp.is_none(), "a gap notice is never stamped");
        let retained = &replay[1..];
        assert_eq!(retained.len(), 2, "{replay:?}");
        assert_eq!(retained[0].seq(), last_missing + 1);
        assert_eq!(asked_ids(retained), ["q3", "q4"]);

        drop((a, b));
        server.stop().await;
    });
}

/// A restart is a new incarnation (a new `server_id`), and a cursor ahead of
/// the head is void: one gap covering everything journaled, then live.
#[test]
fn a_cursor_ahead_of_head_is_void_and_gaps() {
    run_local(async {
        let first = Server::start(None);
        let (_probe, first_id) = connect_as(&first.socket(), "first", full_caps()).await;
        first.stop().await;

        let server = Server::start(None);
        let (mut a, pane) = server.producer("producer").await;
        ask(&mut a, 10, &pane, "q1").await;
        let (mut b, second_id) = connect_as(&server.socket(), "foreign", full_caps()).await;
        let server_id = |hello: &FrameKind| match hello {
            FrameKind::HelloOk { server_id, .. } => server_id.clone(),
            other => panic!("{other:?}"),
        };
        assert_ne!(
            server_id(&first_id),
            server_id(&second_id),
            "a restart is a new incarnation"
        );
        let replay = subscribe(&mut b, 1, None, Some(1_000_000)).await;
        let [gap] = replay.as_slice() else {
            panic!("a void cursor is owed one gap and nothing else: {replay:?}");
        };
        let AgentEvent::JournalGap {
            first_missing,
            last_missing,
        } = gap.event
        else {
            panic!("expected a journal_gap, got {gap:?}");
        };
        assert_eq!(first_missing, 1, "everything this incarnation journaled");
        assert!(last_missing >= 2, "through the head: {last_missing}");
        ask(&mut a, 11, &pane, "q2").await;
        let live = next_event(&mut b, |s| s.asked_id() == Some("q2")).await;
        assert_eq!(
            live.seq(),
            last_missing + 1,
            "live resumes right after the head"
        );

        drop((a, b));
        server.stop().await;
    });
}

/// A live subscriber that stops reading is told of its loss on its own (no
/// later event needed), and every `seq` is either delivered or in a gap.
#[test]
fn a_slow_subscriber_receives_journal_gap_not_silence() {
    run_local(async {
        let server = Server::start(None);
        let mut slow = server.connect("slow").await;
        let _ = subscribe(&mut slow, 1, None, None).await;
        let (mut flooder, pane) = server.producer("flooder").await;
        // ~1 MiB of events: past the socket buffer and the 8-slot mailbox.
        let pad = "x".repeat(3000);
        for n in 0..400 {
            ask_padded(&mut flooder, 100 + n, &pane, format!("flood-{n}"), &pad).await;
        }

        let mut seen = read_until(&mut slow, Seen::is_gap).await;
        seen.extend(drain_until_quiet(&mut slow, Duration::from_millis(500)).await);
        ask(&mut flooder, 9999, &pane, "final").await;
        seen.extend(read_until(&mut slow, |s| s.asked_id() == Some("final")).await);

        let gaps: Vec<(u64, u64)> = seen
            .iter()
            .filter_map(|s| match s.event {
                AgentEvent::JournalGap {
                    first_missing,
                    last_missing,
                } => Some((first_missing, last_missing)),
                _ => None,
            })
            .collect();
        let delivered: Vec<u64> = seen
            .iter()
            .filter(|s| s.stamp.is_some())
            .map(Seen::seq)
            .collect();
        for seq in delivered[0]..=*delivered.last().unwrap() {
            assert!(
                delivered.contains(&seq) || gaps.iter().any(|(lo, hi)| (*lo..=*hi).contains(&seq)),
                "seq {seq} was neither delivered nor reported missing"
            );
        }

        drop((slow, flooder));
        server.stop().await;
    });
}

/// A cursor subscriber that never reads pins nobody (not even its own
/// commands) and receives the whole replay in order once it reads.
#[test]
fn a_cursor_subscriber_that_never_reads_blocks_nobody_and_is_replayed_when_it_reads() {
    run_local(async {
        let server = Server::start(None);
        let (mut producer, pane) = server.producer("producer").await;
        let pad = "y".repeat(2000);
        for n in 0..60 {
            ask_padded(&mut producer, 100 + n, &pane, format!("backlog-{n}"), &pad).await;
        }
        let mut watcher = server.connect("watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;
        let mut stalled = server.connect("stalled").await;
        send_frame(
            &mut stalled,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(0),
            },
        )
        .await;
        send_frame(
            &mut stalled,
            &FrameKind::Command {
                request_id: 7,
                command: report_asked(&pane, "stalled-still-works"),
            },
        )
        .await;
        let _ = next_event(&mut watcher, |s| {
            s.asked_id() == Some("stalled-still-works")
        })
        .await;
        ask(&mut producer, 900, &pane, "others-unaffected").await;

        let mut seen = drain_until_quiet(&mut stalled, Duration::from_millis(500)).await;
        ask(&mut producer, 901, &pane, "final").await;
        seen.extend(read_until(&mut stalled, |s| s.asked_id() == Some("final")).await);
        let backlog: Vec<&str> = asked_ids(&seen)
            .into_iter()
            .filter(|id| id.starts_with("backlog-"))
            .collect();
        let expected: Vec<String> = (0..60).map(|n| format!("backlog-{n}")).collect();
        assert_eq!(backlog, expected, "the whole replay arrives, in order");
        assert_strictly_increasing(&seen);

        drop((producer, watcher, stalled));
        server.stop().await;
    });
}

#[test]
fn resume_after_a_thousand_missed_events_on_a_quiet_server_receives_them_all_in_order() {
    run_local(async {
        const MISSED: u32 = 1000;
        let server = Server::start(None);
        let (mut producer, pane) = server.producer("producer").await;
        for n in 0..MISSED {
            ask(&mut producer, 100 + n, &pane, &format!("missed-{n}")).await;
        }
        let mut resumer = server.connect("resumer").await;
        send_frame(
            &mut resumer,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(0),
            },
        )
        .await;
        let last = format!("missed-{}", MISSED - 1);
        let seen = read_until(&mut resumer, |s| s.asked_id() == Some(last.as_str())).await;
        assert!(
            seen.iter().all(|s| !s.is_gap()),
            "the ring holds everything"
        );
        let expected: Vec<String> = (0..MISSED).map(|n| format!("missed-{n}")).collect();
        assert_eq!(asked_ids(&seen), expected);
        assert_strictly_increasing(&seen);

        drop((producer, resumer));
        server.stop().await;
    });
}

/// Hundreds of OSC-133 marks in one write overflow the 64-event sink; the
/// loss is journaled as a stamped `source_gap` scoped to that pane.
#[test]
fn sink_overflow_journals_source_gap_for_that_terminal() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let marks = "\\033]133;C\\007\\033]133;D;0\\007".repeat(400);
        let seed = gated_seed(&release, &format!("printf '{marks}'; sleep 600"));
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "demo", seed);
        let (mut observer, _) = connect_as(&socket, "observer", full_caps()).await;
        let (mut attacher, _) = connect_as(&socket, "attacher", full_caps()).await;
        let pane = attach_pane(&mut attacher, "demo").await;
        let _ = subscribe(&mut observer, 1, Some(pane.clone()), None).await;
        std::fs::write(&release, b"go").unwrap();

        let reported = next_event(&mut observer, |s| {
            matches!(
                s.event,
                AgentEvent::SourceGap { .. } | AgentEvent::JournalGap { .. }
            )
        })
        .await;
        let source_gap = if matches!(reported.event, AgentEvent::SourceGap { .. }) {
            reported
        } else {
            // The observer's own mailbox overflowed first; the journal still holds it.
            let (mut replayer, _) = connect_as(&socket, "replayer", full_caps()).await;
            subscribe(&mut replayer, 2, Some(pane.clone()), Some(0))
                .await
                .into_iter()
                .find(|s| matches!(s.event, AgentEvent::SourceGap { .. }))
                .expect("the journal holds the source gap")
        };
        assert_eq!(source_gap.terminal.as_ref(), Some(&pane));
        let AgentEvent::SourceGap { dropped } = source_gap.event else {
            unreachable!()
        };
        assert!(dropped > 0);
        assert!(source_gap.stamp.is_some(), "journaled and stamped");

        drop((observer, attacher));
        join_after_shutdown(shutdown, server).await;
    });
}

/// `SUBSCRIBE_RESOURCE_EVENTS` filters are replaced by a later subscribe of
/// either verb; bell, title, and asked reach an empty-filter subscriber.
#[test]
fn resource_event_filters_are_replaced_and_empty_admits_everything() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let seed = gated_seed(
            &release,
            "printf '\\007\\033]2;journal-title\\007'; sleep 600",
        );
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "demo", seed);
        let (mut a, _) = connect_as(&socket, "filtered", full_caps()).await;
        let pane = attach_pane(&mut a, "demo").await;
        let filter = |types: Vec<ResourceEventType>| Command::SubscribeResourceEvents {
            terminal_id: pane.clone(),
            event_types: types,
        };

        assert_eq!(
            command(&mut a, 1, filter(vec![ResourceEventType::CwdChanged]))
                .await
                .0,
            CommandResult::Ok
        );
        let (_, seen) = command(&mut a, 2, report_asked(&pane, "filtered-out")).await;
        assert!(
            asked_ids(&seen).is_empty(),
            "a cwd-only filter admits no asked: {seen:?}"
        );
        let _ = subscribe(&mut a, 3, Some(pane.clone()), None).await;
        let (_, seen) = command(&mut a, 4, report_asked(&pane, "unfiltered-again")).await;
        assert_eq!(
            asked_ids(&seen),
            ["unfiltered-again"],
            "SUBSCRIBE_EVENTS reset the filter"
        );

        assert_eq!(
            command(&mut a, 5, filter(vec![ResourceEventType::CwdChanged]))
                .await
                .0,
            CommandResult::Ok
        );
        assert_eq!(
            command(&mut a, 6, filter(Vec::new())).await.0,
            CommandResult::Ok
        );
        std::fs::write(&release, b"go").unwrap();
        let _ = next_event(&mut a, |s| matches!(s.event, AgentEvent::Bell)).await;
        let _ = next_event(
            &mut a,
            |s| matches!(&s.event, AgentEvent::TitleChanged { title } if title == "journal-title"),
        )
        .await;
        let (_, seen) = command(&mut a, 7, report_asked(&pane, "admitted")).await;
        assert_eq!(
            asked_ids(&seen),
            ["admitted"],
            "the empty filter replaced the old one"
        );

        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

/// Metadata writes (plain, intercepted rename/keep-empty, session create)
/// name the writer as their actor.
#[test]
fn metadata_writes_name_their_actor() {
    run_local(async {
        const KEY: &str = "phux.tui.layout/v1";
        let server = Server::start(None);
        let mut watcher = server.connect("watcher").await;
        let group = Scope::Group(GroupId::new(1));
        for (scope, key) in [
            (group.clone(), KEY),
            (Scope::Global, SESSION_NAME_KEY),
            (Scope::Global, SESSION_KEEP_EMPTY_KEY),
        ] {
            send_frame(
                &mut watcher,
                &FrameKind::SubscribeMetadata {
                    scope,
                    key: key.to_owned(),
                },
            )
            .await;
        }
        let _ = subscribe(&mut watcher, 1, None, None).await;

        let mut writer = server.connect("writer").await;
        let writes = [
            (group, KEY, b"layout".to_vec()),
            (Scope::Global, SESSION_NAME_KEY, b"demo\0renamed".to_vec()),
            (
                Scope::Global,
                SESSION_KEEP_EMPTY_KEY,
                phux_protocol::wire::frame::encode_session_keep_empty("renamed", true),
            ),
        ];
        for (n, (scope, key, value)) in writes.into_iter().enumerate() {
            send_frame(
                &mut writer,
                &FrameKind::SetMetadata {
                    request_id: u32::try_from(n).unwrap(),
                    scope,
                    key: key.to_owned(),
                    value,
                },
            )
            .await;
            let actor = recv_until(&mut watcher, |_, frame| match frame {
                FrameKind::MetadataChanged {
                    key: changed,
                    actor,
                    ..
                } if changed == key => Some(actor),
                _ => None,
            })
            .await;
            assert_eq!(
                actor.expect(key).client_name.as_deref(),
                Some("writer"),
                "{key}"
            );
        }

        send_frame(
            &mut writer,
            &FrameKind::SetMetadata {
                request_id: 9,
                scope: Scope::Global,
                key: SESSION_CREATE_KEY.to_owned(),
                value: br#"{"name":"made-by-writer"}"#.to_vec(),
            },
        )
        .await;
        let spawned = next_event(&mut watcher, |s| {
            matches!(s.event, AgentEvent::ResourceSpawned { .. })
        })
        .await;
        assert_eq!(
            spawned.actor_name(),
            Some("writer"),
            "a created session's seed pane"
        );

        drop((watcher, writer));
        server.stop().await;
    });
}

/// Input-lease take and give are journaled naming the driver, in stamp and body.
#[test]
fn terminal_control_events_are_journaled_with_actor() {
    run_local(async {
        let server = Server::start(None);
        let mut watcher = server.connect("watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;
        let (mut writer, pane) = server.producer("writer").await;
        let acquire = Command::AcquireInput {
            terminal_id: pane.clone(),
            mode: InputMode::Cooperative,
            ttl_ms: 0,
        };
        assert_eq!(command(&mut writer, 20, acquire).await.0, CommandResult::Ok);
        let control = |action: ControlAction| move |s: &Seen| matches!(s.event, AgentEvent::TerminalControl { action: got, .. } if got == action);
        let taken = next_event(&mut watcher, control(ControlAction::Acquired)).await;
        let release = Command::ReleaseInput {
            terminal_id: pane.clone(),
        };
        assert_eq!(command(&mut writer, 21, release).await.0, CommandResult::Ok);
        let given = next_event(&mut watcher, control(ControlAction::Released)).await;
        for seen in [&taken, &given] {
            assert_eq!(seen.terminal.as_ref(), Some(&pane));
            assert_eq!(seen.actor_name(), Some("writer"));
            let AgentEvent::TerminalControl { actor, .. } = &seen.event else {
                unreachable!()
            };
            let stamped = seen.stamp.as_ref().unwrap().actor.as_ref().unwrap();
            assert_eq!(Some(stamped.client), *actor, "stamp and body agree");
        }
        assert!(taken.seq() < given.seq());

        drop((watcher, writer));
        server.stop().await;
    });
}

/// For panes that exit at once after emitting marks: `pane_spawned` precedes
/// `pane_closed`, and nothing about a pane is journaled after its close.
#[test]
fn fast_exiting_panes_journal_their_events_between_spawned_and_closed() {
    run_local(async {
        let server = Server::start(None);
        let (mut a, _) = server.producer("spawner").await;
        let _ = subscribe(&mut a, 1, None, None).await;
        let mut spawned = Vec::new();
        for n in 0..12 {
            spawned.push(
                spawn_pane(
                    &mut a,
                    500 + n,
                    "printf '\\033]133;C\\007\\033]133;D;0\\007'",
                )
                .await
                .0,
            );
        }
        let is_closed = |seen: &[Seen], pane: &ResourceId| {
            seen.iter().any(|s| {
                s.terminal.as_ref() == Some(pane)
                    && matches!(s.event, AgentEvent::ResourceClosed { .. })
            })
        };
        let mut seen = Vec::new();
        for _ in 0..60 {
            seen = journal_contents(&server.socket()).await;
            if spawned.iter().all(|pane| is_closed(&seen, pane)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(
            spawned.iter().all(|pane| is_closed(&seen, pane)),
            "{spawned:?}"
        );
        assert!(
            seen.iter()
                .any(|s| matches!(s.event, AgentEvent::CommandFinished { .. })),
            "final marks journaled"
        );
        for close in seen
            .iter()
            .filter(|s| matches!(s.event, AgentEvent::ResourceClosed { .. }))
        {
            let pane = close.terminal.as_ref().expect("a close names its pane");
            let about: Vec<&Seen> = seen
                .iter()
                .filter(|s| s.terminal.as_ref() == Some(pane))
                .collect();
            let opened = about
                .iter()
                .find(|s| matches!(s.event, AgentEvent::ResourceSpawned { .. }))
                .unwrap_or_else(|| panic!("{pane:?} closed without a pane_spawned"));
            assert!(opened.seq() < close.seq(), "{pane:?}");
            assert!(
                about.iter().all(|s| s.seq() <= close.seq()),
                "{pane:?}: {about:?}"
            );
        }

        drop(a);
        server.stop().await;
    });
}

/// A spawner that vanishes mid-publication still leaves exactly one
/// `pane_closed` for every pane that is gone, and none for a live one.
#[test]
fn a_spawn_abandoned_mid_publication_still_journals_its_close() {
    run_local(async {
        let server = Server::start(None);
        let mut watcher = server.connect("watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;
        for n in 0..15 {
            let (mut spawner, _) = server.producer("abandoner").await;
            send_frame(
                &mut spawner,
                &Spawn::command(&["/bin/sh", "-c", "sleep 600"]).frame(700 + n),
            )
            .await;
            drop(spawner);
        }
        let seen = drain_until_quiet(&mut watcher, Duration::from_millis(1500)).await;
        let spawned: Vec<ResourceId> = seen
            .iter()
            .filter(|s| matches!(s.event, AgentEvent::ResourceSpawned { .. }))
            .filter_map(|s| s.terminal.clone())
            .collect();
        for (n, pane) in (900..).zip(&spawned) {
            let closes = seen
                .iter()
                .filter(|s| {
                    s.terminal.as_ref() == Some(pane)
                        && matches!(s.event, AgentEvent::ResourceClosed { .. })
                })
                .count();
            let state = Command::GetTerminalState {
                terminal_id: pane.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            };
            let (alive, _) = command(&mut watcher, n, state).await;
            let gone = usize::from(matches!(alive, CommandResult::Error { .. }));
            assert_eq!(closes, gone, "{pane:?}");
        }

        drop(watcher);
        server.stop().await;
    });
}
