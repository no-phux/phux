//! Wire-level integration tests for the event journal (ADR-0123,
//! `docs/spec/L1.md` §7.3): every event is stamped with one server-wide
//! sequence, a cursor subscription is replayed from the journal, every loss
//! is a typed gap, both subscribe verbs share one registry, and the actor
//! behind an event or a metadata change is named.
//!
//! Most tests run against a no-PTY seed pane and drive events with
//! `REPORT_ASKED`, which journals an `asked` event and answers the command
//! only after the event is queued, so "events before the reply" is exact
//! rather than timed. `SUBSCRIBE_EVENTS` has no reply of its own; a
//! `GET_STATE` sent after it is the barrier, because one connection's frames
//! are processed in order and a cursor replay is sent before the frame loop
//! moves on.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::Path;
use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ControlAction, EventStamp, FrameKind, InputMode,
    ResourceEventType, Scope, SpawnResult, StateScope, TYPE_ATTACHED, TYPE_HELLO_OK,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, join_after_shutdown, recv_typed,
    recv_until, run_local, send_frame, spawn_server_with, spawn_server_with_seed_cmd,
    wait_for_raw_socket,
};

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

/// `HELLO` as `name`; returns `HELLO_OK.server_id`.
async fn hello(stream: &mut UnixStream, name: &str) -> Vec<u8> {
    send_frame(
        stream,
        &FrameKind::Hello {
            client_name: name.to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK);
    let FrameKind::HelloOk { server_id, .. } = frame else {
        panic!("expected HELLO_OK, got {frame:?}");
    };
    server_id
}

async fn connect(socket: &Path, name: &str) -> (UnixStream, Vec<u8>) {
    let mut stream = wait_for_raw_socket(socket, SOCKET_CONNECT_DEADLINE).await;
    let server_id = hello(&mut stream, name).await;
    (stream, server_id)
}

/// Attach to `session`; returns its focused pane.
async fn attach_pane(stream: &mut UnixStream, session: &str) -> ResourceId {
    send_frame(stream, &attach_by_name(session)).await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED);
    let FrameKind::Attached { snapshot, .. } = frame else {
        panic!("expected ATTACHED, got {frame:?}");
    };
    snapshot.focused_resource
}

/// Every `EVENT` that arrives before the reply to `request_id`.
async fn events_until_result(
    stream: &mut UnixStream,
    request_id: u32,
) -> (CommandResult, Vec<Seen>) {
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
    events_until_result(stream, request_id).await
}

/// `SUBSCRIBE_EVENTS`, then the `GET_STATE` barrier. Returns what the
/// subscription was sent ahead of the barrier: its replay, when it named a
/// cursor.
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
    let (result, seen) = command(
        stream,
        request_id,
        Command::GetState {
            scope: StateScope::Server,
        },
    )
    .await;
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

/// `REPORT_ASKED` as a command whose only interest is the event it causes.
async fn ask(stream: &mut UnixStream, request_id: u32, terminal: &ResourceId, id: &str) {
    let (result, _) = command(stream, request_id, report_asked(terminal, id)).await;
    assert_eq!(result, CommandResult::Ok, "REPORT_ASKED {id}");
}

/// The next `EVENT` `matches` accepts.
async fn next_event(stream: &mut UnixStream, matches: impl Fn(&Seen) -> bool + Send) -> Seen {
    recv_until(stream, |_, frame| {
        as_seen(frame).filter(|seen| matches(seen))
    })
    .await
}

/// `SPAWN_RESOURCE` a parked pane from a subscribed client; returns its id
/// and its `pane_spawned`.
async fn spawn_pane(stream: &mut UnixStream, request_id: u32) -> (ResourceId, Seen) {
    let (id, before_reply) = spawn_pane_id(stream, request_id).await;
    let is_announcement = |seen: &Seen| {
        seen.terminal.as_ref() == Some(&id)
            && matches!(seen.event, AgentEvent::ResourceSpawned { .. })
    };
    let announcement = match before_reply.into_iter().find(|seen| is_announcement(seen)) {
        Some(seen) => seen,
        None => next_event(stream, is_announcement).await,
    };
    (id, announcement)
}

/// `SPAWN_RESOURCE` a parked pane; returns its id once the reply arrives,
/// with every `EVENT` that arrived ahead of it.
async fn spawn_pane_id(stream: &mut UnixStream, request_id: u32) -> (ResourceId, Vec<Seen>) {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "sleep 600".to_owned(),
            ]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        },
    )
    .await;
    let mut before_reply = Vec::new();
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
                return (id, before_reply);
            }
            other => before_reply.extend(as_seen(other)),
        }
    }
}

/// A PTY seed that waits for `release` before running `script`, then parks.
fn gated_seed(release: &Path, script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(format!(
        "until [ -f '{}' ]; do sleep 0.01; done; {script}; sleep 600",
        release.display(),
    ));
    cmd
}

#[test]
fn events_carry_monotone_seq_ts_and_actor_across_resources() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "orchestrator").await;
        let first = attach_pane(&mut a, "demo").await;
        let _ = subscribe(&mut a, 1, None, None).await;

        let (second, spawned) = spawn_pane(&mut a, 2).await;
        let (_, one) = command(&mut a, 3, report_asked(&first, "on-first")).await;
        let (_, two) = command(&mut a, 4, report_asked(&second, "on-second")).await;
        let asked: Vec<Seen> = one
            .into_iter()
            .chain(two)
            .filter(|seen| seen.asked_id().is_some())
            .collect();
        assert_eq!(asked.len(), 2, "one asked per pane: {asked:?}");
        assert_eq!(asked[0].terminal.as_ref(), Some(&first));
        assert_eq!(asked[1].terminal.as_ref(), Some(&second));

        let ordered = [&spawned, &asked[0], &asked[1]];
        for pair in ordered.windows(2) {
            assert!(
                pair[0].seq() < pair[1].seq(),
                "one server-wide order across resources: {pair:?}"
            );
        }
        for seen in ordered {
            let ts = seen.stamp.as_ref().unwrap().ts_ms;
            assert!(ts > 1_600_000_000_000, "ts_ms is Unix milliseconds: {ts}");
        }
        assert_eq!(
            spawned.actor_name(),
            Some("orchestrator"),
            "the spawn is attributed to the connection that asked for it"
        );

        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn client_name_from_hello_is_the_actor_label() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut watcher, _) = connect(&socket, "watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;
        let (mut labeler, _) = connect(&socket, "labeler").await;
        let _ = attach_pane(&mut labeler, "demo").await;
        // The labeler is not subscribed; the watcher sees the announcement.
        let (spawned_id, _) = spawn_pane_id(&mut labeler, 2).await;

        let seen = next_event(&mut watcher, |seen| {
            seen.terminal.as_ref() == Some(&spawned_id)
                && matches!(seen.event, AgentEvent::ResourceSpawned { .. })
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

        drop((watcher, labeler));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn subscribe_with_after_seq_replays_missed_events_in_order_then_goes_live() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut a, "demo").await;
        for (n, id) in ["q1", "q2", "q3"].into_iter().enumerate() {
            ask(&mut a, 10 + u32::try_from(n).unwrap(), &pane, id).await;
        }

        let (mut b, _) = connect(&socket, "late").await;
        let replay = subscribe(&mut b, 1, Some(pane.clone()), Some(0)).await;
        assert!(
            replay
                .iter()
                .all(|seen| !matches!(seen.event, AgentEvent::JournalGap { .. })),
            "the journal still holds everything: {replay:?}"
        );
        let ids: Vec<&str> = replay.iter().filter_map(Seen::asked_id).collect();
        assert_eq!(ids, ["q1", "q2", "q3"], "replayed in order");
        for pair in replay.windows(2) {
            assert!(pair[0].seq() < pair[1].seq(), "seq order: {pair:?}");
        }
        let last = replay.last().unwrap().seq();

        ask(&mut a, 20, &pane, "q4").await;
        let live = next_event(&mut b, |seen| seen.asked_id() == Some("q4")).await;
        assert!(live.seq() > last, "live continues after the replay");

        drop((a, b));
        join_after_shutdown(shutdown, server).await;
    });
}

/// A `GET_STATE` answer names the journal head at its cut (L1 §7.3): the
/// newest `seq` stamped before the snapshot. A server-wide subscriber sees
/// every journaled event before the answer, so the head is the largest
/// `seq` that subscriber has seen by then.
#[test]
fn get_state_reports_the_journal_head_at_the_snapshot_cut() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut a, "demo").await;
        let mut seen = subscribe(&mut a, 1, None, None).await;
        let (_, asked) = command(&mut a, 2, report_asked(&pane, "q1")).await;
        seen.extend(asked);
        let (result, before_cut) = command(
            &mut a,
            3,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await;
        seen.extend(before_cut);
        let CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(snapshot)) =
            result
        else {
            panic!("GET_STATE failed: {result:?}");
        };
        let newest = seen
            .iter()
            .filter(|event| event.stamp.is_some())
            .map(Seen::seq)
            .max()
            .expect("the asked event was journaled");
        assert!(seen.iter().any(|event| event.asked_id() == Some("q1")));
        assert_eq!(
            snapshot.journal_head(),
            Some(newest),
            "the head is the newest seq stamped before the cut"
        );

        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

fn journal_head_of(result: CommandResult) -> u64 {
    let CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(snapshot)) = result
    else {
        panic!("GET_STATE failed: {result:?}");
    };
    snapshot
        .journal_head()
        .expect("an EVENT_JOURNAL server writes the head")
}

/// L1 §7.3: the head is per connection. A connection watching one terminal
/// gets the newest `seq` its subscription admits, so events on another
/// terminal never hold its catch-up open; a connection without
/// subscriptions gets the newest `seq` assigned.
#[test]
fn the_journal_head_reflects_only_what_the_connection_admits() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut a, "demo").await;
        let (mut b, _) = connect(&socket, "watcher").await;
        let mut watched = subscribe(&mut b, 1, Some(pane.clone()), None).await;
        ask(&mut a, 2, &pane, "q1").await;
        watched.push(next_event(&mut b, |seen| seen.asked_id() == Some("q1")).await);
        let (other, _) = spawn_pane_id(&mut a, 3).await;
        ask(&mut a, 4, &other, "q2").await;

        let get_state = || Command::GetState {
            scope: StateScope::Server,
        };
        let (result, before_cut) = command(&mut b, 2, get_state()).await;
        watched.extend(before_cut);
        let watcher_head = journal_head_of(result);
        let (mut c, _) = connect(&socket, "bystander").await;
        let global_head = journal_head_of(command(&mut c, 1, get_state()).await.0);
        let q2 = journal_contents(&socket)
            .await
            .iter()
            .find(|seen| seen.asked_id() == Some("q2"))
            .map(Seen::seq)
            .expect("q2 was journaled");

        let newest_watched = watched
            .iter()
            .filter(|seen| seen.stamp.is_some())
            .map(Seen::seq)
            .max()
            .expect("the watcher saw q1");
        assert_eq!(
            watcher_head, newest_watched,
            "the head is the newest seq the watcher's subscription admits"
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
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn subscribe_with_a_stale_cursor_receives_journal_gap_first() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |cfg| {
            cfg.event_journal_entries = 2;
        });
        let (mut a, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut a, "demo").await;
        for (n, id) in ["q1", "q2", "q3", "q4"].into_iter().enumerate() {
            ask(&mut a, 10 + u32::try_from(n).unwrap(), &pane, id).await;
        }

        let (mut b, _) = connect(&socket, "stale").await;
        let replay = subscribe(&mut b, 1, None, Some(0)).await;
        let Some(AgentEvent::JournalGap {
            first_missing,
            last_missing,
        }) = replay.first().map(|seen| seen.event.clone())
        else {
            panic!("the first frame is the gap: {replay:?}");
        };
        assert_eq!(first_missing, 1, "everything from the cursor on");
        assert!(replay[0].stamp.is_none(), "a gap notice is never stamped");
        let retained = &replay[1..];
        assert_eq!(retained.len(), 2, "the ring holds two events: {replay:?}");
        assert_eq!(
            retained[0].seq(),
            last_missing + 1,
            "the replay resumes after the gap"
        );
        let ids: Vec<&str> = retained.iter().filter_map(Seen::asked_id).collect();
        assert_eq!(ids, ["q3", "q4"]);

        drop((a, b));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_cursor_ahead_of_head_is_void_and_gaps() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut a, "demo").await;
        ask(&mut a, 10, &pane, "q1").await;

        let (mut b, _) = connect(&socket, "foreign").await;
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
        let live = next_event(&mut b, |seen| seen.asked_id() == Some("q2")).await;
        assert_eq!(
            live.seq(),
            last_missing + 1,
            "live resumes right after the head"
        );

        drop((a, b));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn cursor_from_a_previous_incarnation_is_refused_with_gap() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let first_socket = tmp.path().join("first.sock");
        let (shutdown, server) = spawn_server_with(first_socket.clone(), Some("demo"), |_| {});
        let (mut a, first_id) = connect(&first_socket, "first").await;
        let pane = attach_pane(&mut a, "demo").await;
        let _ = subscribe(&mut a, 1, None, None).await;
        let mut cursor = 0;
        for (n, id) in ["q1", "q2", "q3", "q4", "q5"].into_iter().enumerate() {
            let (_, seen) = command(
                &mut a,
                10 + u32::try_from(n).unwrap(),
                report_asked(&pane, id),
            )
            .await;
            cursor = seen.iter().map(Seen::seq).max().unwrap_or(cursor);
        }
        drop(a);
        join_after_shutdown(shutdown, server).await;

        let second_socket = tmp.path().join("second.sock");
        let (shutdown, server) = spawn_server_with(second_socket.clone(), Some("demo"), |_| {});
        let (mut b, second_id) = connect(&second_socket, "resumer").await;
        assert_ne!(first_id, second_id, "a restart is a new incarnation");
        let replay = subscribe(&mut b, 1, None, Some(cursor)).await;
        assert!(
            matches!(
                replay.as_slice(),
                [Seen {
                    event: AgentEvent::JournalGap {
                        first_missing: 1,
                        ..
                    },
                    stamp: None,
                    ..
                }]
            ),
            "the old cursor ({cursor}) is void here: {replay:?}"
        );

        drop(b);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_slow_subscriber_receives_journal_gap_not_silence() {
    run_local(async {
        const FLOOD: u32 = 400;
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut slow, _) = connect(&socket, "slow").await;
        let _ = subscribe(&mut slow, 1, None, None).await;
        let (mut flooder, _) = connect(&socket, "flooder").await;
        let pane = attach_pane(&mut flooder, "demo").await;

        // The slow client reads nothing while ~1 MiB of events is produced:
        // far past its socket buffer and its eight-slot mailbox.
        let filler = "x".repeat(3000);
        for n in 0..FLOOD {
            let report = Command::ReportAsked {
                terminal_id: pane.clone(),
                id: format!("flood-{n}"),
                question: format!("{n} {filler}"),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            };
            // The flooder is not subscribed: only its own reply comes back.
            let (result, _) = command(&mut flooder, 100 + n, report).await;
            assert_eq!(result, CommandResult::Ok);
        }

        let mut seen = drain_until_quiet(&mut slow, Duration::from_millis(500)).await;
        ask(&mut flooder, 9999, &pane, "final").await;
        loop {
            let frame = next_event(&mut slow, |_| true).await;
            let done = frame.asked_id() == Some("final");
            seen.push(frame);
            if done {
                break;
            }
        }

        let gaps: Vec<(u64, u64)> = seen
            .iter()
            .filter_map(|seen| match seen.event {
                AgentEvent::JournalGap {
                    first_missing,
                    last_missing,
                } => Some((first_missing, last_missing)),
                _ => None,
            })
            .collect();
        assert!(!gaps.is_empty(), "a slow reader is told it missed events");
        let delivered: Vec<u64> = seen
            .iter()
            .filter(|seen| seen.stamp.is_some())
            .map(Seen::seq)
            .collect();
        let first = *delivered.first().unwrap();
        let last = *delivered.last().unwrap();
        for seq in first..=last {
            let accounted =
                delivered.contains(&seq) || gaps.iter().any(|(lo, hi)| (*lo..=*hi).contains(&seq));
            assert!(
                accounted,
                "seq {seq} was neither delivered nor reported missing"
            );
        }

        drop((slow, flooder));
        join_after_shutdown(shutdown, server).await;
    });
}

/// Every `EVENT` until the stream has been quiet for `quiet`.
async fn drain_until_quiet(stream: &mut UnixStream, quiet: Duration) -> Vec<Seen> {
    let mut seen = Vec::new();
    while let Ok((_, frame)) = timeout(quiet, recv_typed(stream)).await {
        seen.extend(as_seen(frame));
    }
    seen
}

#[test]
fn sink_overflow_journals_source_gap_for_that_terminal() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        // Hundreds of OSC-133 command marks in one write: each becomes an
        // event in the same actor turn, far past the 64-event sink.
        let marks = "\\033]133;C\\007\\033]133;D;0\\007".repeat(400);
        let cmd = gated_seed(&release, &format!("printf '{marks}'"));
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "demo", cmd);
        let (mut observer, _) = connect(&socket, "observer").await;
        let pane = {
            let (mut attacher, _) = connect(&socket, "attacher").await;
            attach_pane(&mut attacher, "demo").await
        };
        let _ = subscribe(&mut observer, 1, Some(pane.clone()), None).await;
        std::fs::write(&release, b"go").unwrap();

        let reported = next_event(&mut observer, |seen| {
            matches!(
                seen.event,
                AgentEvent::SourceGap { .. } | AgentEvent::JournalGap { .. }
            )
        })
        .await;
        let source_gap = if matches!(reported.event, AgentEvent::SourceGap { .. }) {
            reported
        } else {
            // The observer's own mailbox overflowed first; the journal still
            // holds the source gap, so a cursor replay finds it.
            let (mut replayer, _) = connect(&socket, "replayer").await;
            let replay = subscribe(&mut replayer, 2, Some(pane.clone()), Some(0)).await;
            replay
                .into_iter()
                .find(|seen| matches!(seen.event, AgentEvent::SourceGap { .. }))
                .expect("the journal holds the source gap")
        };
        assert_eq!(
            source_gap.terminal.as_ref(),
            Some(&pane),
            "scoped to the pane"
        );
        let AgentEvent::SourceGap { dropped } = source_gap.event else {
            unreachable!()
        };
        assert!(dropped > 0);
        assert!(
            source_gap.stamp.is_some(),
            "a source gap is journaled and stamped"
        );

        drop(observer);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn subscribe_resource_events_and_subscribe_events_deliver_each_event_once() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "both-ways").await;
        let pane = attach_pane(&mut a, "demo").await;
        send_frame(
            &mut a,
            &FrameKind::SubscribeEvents {
                terminal: Some(pane.clone()),
                after_seq: None,
            },
        )
        .await;
        let (result, _) = command(
            &mut a,
            1,
            Command::SubscribeResourceEvents {
                terminal_id: pane.clone(),
                event_types: Vec::new(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = subscribe(&mut a, 2, None, None).await;

        let (_, seen) = command(&mut a, 3, report_asked(&pane, "once")).await;
        let asked = seen
            .iter()
            .filter(|seen| seen.asked_id() == Some("once"))
            .count();
        assert_eq!(asked, 1, "three overlapping scopes, one delivery: {seen:?}");

        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn resource_events_filter_is_replaced_on_resubscribe() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "filtered").await;
        let pane = attach_pane(&mut a, "demo").await;
        let subscribe_filtered = |types: Vec<ResourceEventType>| Command::SubscribeResourceEvents {
            terminal_id: pane.clone(),
            event_types: types,
        };

        let (result, _) = command(
            &mut a,
            1,
            subscribe_filtered(vec![ResourceEventType::CwdChanged]),
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let (_, seen) = command(&mut a, 2, report_asked(&pane, "filtered-out")).await;
        assert!(
            seen.iter().all(|seen| seen.asked_id().is_none()),
            "a cwd-only filter admits no asked: {seen:?}"
        );

        let (result, _) = command(&mut a, 3, subscribe_filtered(Vec::new())).await;
        assert_eq!(result, CommandResult::Ok);
        let (_, seen) = command(&mut a, 4, report_asked(&pane, "admitted")).await;
        let ids: Vec<&str> = seen.iter().filter_map(Seen::asked_id).collect();
        assert_eq!(ids, ["admitted"], "the empty filter replaced the old one");

        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn bell_title_and_asked_reach_a_filtered_0x0d_subscriber_with_an_empty_filter() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let cmd = gated_seed(&release, "printf '\\007\\033]2;journal-title\\007'");
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "demo", cmd);
        let (mut a, _) = connect(&socket, "resource-subscriber").await;
        let pane = attach_pane(&mut a, "demo").await;
        let (result, _) = command(
            &mut a,
            1,
            Command::SubscribeResourceEvents {
                terminal_id: pane.clone(),
                event_types: Vec::new(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok, "registered before the reply");
        std::fs::write(&release, b"go").unwrap();

        let _ = next_event(&mut a, |seen| matches!(seen.event, AgentEvent::Bell)).await;
        let _ = next_event(&mut a, |seen| {
            matches!(&seen.event, AgentEvent::TitleChanged { title } if title == "journal-title")
        })
        .await;
        let (_, seen) = command(&mut a, 2, report_asked(&pane, "reaches-0x0d")).await;
        assert!(
            seen.iter()
                .any(|seen| seen.asked_id() == Some("reaches-0x0d")),
            "asked reaches a SUBSCRIBE_RESOURCE_EVENTS subscriber: {seen:?}"
        );

        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn metadata_changed_carries_the_writers_actor() {
    run_local(async {
        const KEY: &str = "phux.tui.layout/v1";
        let scope = Scope::Group(GroupId::new(1));
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut watcher, _) = connect(&socket, "watcher").await;
        send_frame(
            &mut watcher,
            &FrameKind::SubscribeMetadata {
                scope: scope.clone(),
                key: KEY.to_owned(),
            },
        )
        .await;
        let _ = subscribe(&mut watcher, 1, None, None).await;

        let (mut writer, _) = connect(&socket, "writer").await;
        send_frame(
            &mut writer,
            &FrameKind::SetMetadata {
                request_id: 1,
                scope,
                key: KEY.to_owned(),
                value: b"layout".to_vec(),
            },
        )
        .await;

        let actor = recv_until(&mut watcher, |_, frame| match frame {
            FrameKind::MetadataChanged { key, actor, .. } if key == KEY => Some(actor),
            _ => None,
        })
        .await;
        let actor = actor.expect("a client's write is attributed");
        assert_eq!(actor.client_name.as_deref(), Some("writer"));

        drop((watcher, writer));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn terminal_control_events_are_journaled_with_actor() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut watcher, _) = connect(&socket, "watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;
        let (mut driver, _) = connect(&socket, "driver").await;
        let pane = attach_pane(&mut driver, "demo").await;

        let (result, _) = command(
            &mut driver,
            2,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 0,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let taken = next_event(&mut watcher, |seen| {
            matches!(
                seen.event,
                AgentEvent::TerminalControl {
                    action: ControlAction::Acquired,
                    ..
                }
            )
        })
        .await;

        let (result, _) = command(
            &mut driver,
            3,
            Command::ReleaseInput {
                terminal_id: pane.clone(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let given = next_event(&mut watcher, |seen| {
            matches!(
                seen.event,
                AgentEvent::TerminalControl {
                    action: ControlAction::Released,
                    ..
                }
            )
        })
        .await;

        for seen in [&taken, &given] {
            assert_eq!(seen.terminal.as_ref(), Some(&pane));
            assert_eq!(
                seen.actor_name(),
                Some("driver"),
                "take and give name the driver"
            );
            let AgentEvent::TerminalControl { actor, .. } = &seen.event else {
                unreachable!()
            };
            let stamped = seen.stamp.as_ref().unwrap().actor.as_ref().unwrap();
            assert_eq!(Some(stamped.client), *actor, "stamp and body agree");
        }
        assert!(taken.seq() < given.seq());

        drop((watcher, driver));
        join_after_shutdown(shutdown, server).await;
    });
}

/// Every event the journal still holds, read by a fresh connection's
/// cursor replay: loss-free however busy the server was, because the replay
/// is pulled from the ring as this reader takes it (ADR-0123).
async fn journal_contents(socket: &Path) -> Vec<Seen> {
    let (mut reader, _) = connect(socket, "journal-reader").await;
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

fn has_closed(seen: &[Seen], pane: &ResourceId) -> bool {
    seen.iter().any(|seen| {
        seen.terminal.as_ref() == Some(pane)
            && matches!(seen.event, AgentEvent::ResourceClosed { .. })
    })
}

/// Spawn `count` panes running `script` from `stream`, then return the
/// journal once every one of them has closed.
async fn spawn_short_lived(
    stream: &mut UnixStream,
    socket: &Path,
    script: &str,
    count: u32,
) -> Vec<Seen> {
    let mut spawned = Vec::new();
    for n in 0..count {
        spawned.push(spawn_pane_running(stream, 500 + n, script).await);
    }
    for _ in 0..60 {
        let seen = journal_contents(socket).await;
        if spawned.iter().all(|pane| has_closed(&seen, pane)) {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("not every short-lived pane closed: {spawned:?}");
}

/// `SPAWN_RESOURCE` `/bin/sh -c script`; returns its id once the reply
/// arrives.
async fn spawn_pane_running(stream: &mut UnixStream, request_id: u32, script: &str) -> ResourceId {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                script.to_owned(),
            ]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        },
    )
    .await;
    recv_until(stream, |_, frame| match frame {
        FrameKind::ResourceSpawned {
            request_id: got,
            result,
        } if got == request_id => {
            let SpawnResult::Ok(id) = result else {
                panic!("spawn failed: {result:?}");
            };
            Some(id)
        }
        _ => None,
    })
    .await
}

/// For every pane that closed in `seen`: its `pane_spawned` came first and
/// nothing about it took a later `seq` than its `pane_closed`.
fn assert_causal_per_pane(seen: &[Seen]) {
    let closes = seen
        .iter()
        .filter(|seen| matches!(seen.event, AgentEvent::ResourceClosed { .. }));
    for close in closes {
        let pane = close.terminal.as_ref().expect("a close names its pane");
        let about: Vec<&Seen> = seen
            .iter()
            .filter(|seen| seen.terminal.as_ref() == Some(pane))
            .collect();
        let spawned = about
            .iter()
            .find(|seen| matches!(seen.event, AgentEvent::ResourceSpawned { .. }))
            .unwrap_or_else(|| panic!("{pane:?} closed without a pane_spawned: {about:?}"));
        assert!(
            spawned.seq() < close.seq(),
            "pane_spawned({}) must precede pane_closed({}) for {pane:?}",
            spawned.seq(),
            close.seq()
        );
        for event in &about {
            assert!(
                event.seq() <= close.seq(),
                "{:?} for {pane:?} took seq {} after its pane_closed({})",
                event.event,
                event.seq(),
                close.seq()
            );
        }
    }
}

#[test]
fn fast_exiting_panes_journal_spawned_before_closed() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "spawner").await;
        let _ = attach_pane(&mut a, "demo").await;
        let _ = subscribe(&mut a, 1, None, None).await;
        let seen = spawn_short_lived(&mut a, &socket, "exit 0", 12).await;
        assert_causal_per_pane(&seen);
        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_pane_journals_its_last_events_before_its_close() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "spawner").await;
        let _ = attach_pane(&mut a, "demo").await;
        let _ = subscribe(&mut a, 1, None, None).await;
        let seen = spawn_short_lived(
            &mut a,
            &socket,
            "printf '\\033]133;C\\007\\033]133;D;0\\007'",
            12,
        )
        .await;
        assert!(
            seen.iter()
                .any(|seen| matches!(seen.event, AgentEvent::CommandFinished { .. })),
            "the panes' final command marks were journaled: {seen:?}"
        );
        assert_causal_per_pane(&seen);
        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_cursor_subscriber_that_never_reads_blocks_nobody_and_is_replayed_when_it_reads() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut producer, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut producer, "demo").await;
        let filler = "y".repeat(2000);
        for n in 0..60 {
            let report = Command::ReportAsked {
                terminal_id: pane.clone(),
                id: format!("backlog-{n}"),
                question: format!("{n} {filler}"),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            };
            let (result, _) = command(&mut producer, 100 + n, report).await;
            assert_eq!(result, CommandResult::Ok);
        }
        let (mut watcher, _) = connect(&socket, "watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;

        // The stalled client asks for the whole backlog and reads nothing.
        let (mut stalled, _) = connect(&socket, "stalled").await;
        send_frame(
            &mut stalled,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(0),
            },
        )
        .await;
        // Its own next command still runs: its frame loop is not pinned.
        send_frame(
            &mut stalled,
            &FrameKind::Command {
                request_id: 7,
                command: report_asked(&pane, "stalled-still-works"),
            },
        )
        .await;
        let _ = next_event(&mut watcher, |seen| {
            seen.asked_id() == Some("stalled-still-works")
        })
        .await;
        // And everyone else is unaffected.
        ask(&mut producer, 900, &pane, "others-unaffected").await;

        let mut seen = drain_until_quiet(&mut stalled, Duration::from_millis(500)).await;
        ask(&mut producer, 901, &pane, "final").await;
        loop {
            let frame = next_event(&mut stalled, |_| true).await;
            let done = frame.asked_id() == Some("final");
            seen.push(frame);
            if done {
                break;
            }
        }
        let backlog: Vec<&str> = seen
            .iter()
            .filter_map(Seen::asked_id)
            .filter(|id| id.starts_with("backlog-"))
            .collect();
        let expected: Vec<String> = (0..60).map(|n| format!("backlog-{n}")).collect();
        assert_eq!(
            backlog, expected,
            "the whole replay arrives, in order, once it reads"
        );
        assert_strictly_increasing(&seen);

        drop((producer, watcher, stalled));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn rename_and_keep_empty_carry_the_writers_actor() {
    run_local(async {
        use phux_protocol::wire::frame::{SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY};
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut watcher, _) = connect(&socket, "watcher").await;
        for key in [SESSION_NAME_KEY, SESSION_KEEP_EMPTY_KEY] {
            send_frame(
                &mut watcher,
                &FrameKind::SubscribeMetadata {
                    scope: Scope::Global,
                    key: key.to_owned(),
                },
            )
            .await;
        }
        let _ = subscribe(&mut watcher, 1, None, None).await;

        let (mut writer, _) = connect(&socket, "renamer").await;
        let intercepted = [
            (SESSION_NAME_KEY, b"demo\0renamed".to_vec()),
            (
                SESSION_KEEP_EMPTY_KEY,
                phux_protocol::wire::frame::encode_session_keep_empty("renamed", true),
            ),
        ];
        for (n, (key, value)) in intercepted.into_iter().enumerate() {
            send_frame(
                &mut writer,
                &FrameKind::SetMetadata {
                    request_id: u32::try_from(n).unwrap(),
                    scope: Scope::Global,
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
            let actor = actor.unwrap_or_else(|| panic!("{key} change is attributed"));
            assert_eq!(actor.client_name.as_deref(), Some("renamer"), "{key}");
        }

        drop((watcher, writer));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn subscribe_events_resets_a_resource_events_filter() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut a, _) = connect(&socket, "resetter").await;
        let pane = attach_pane(&mut a, "demo").await;
        let (result, _) = command(
            &mut a,
            1,
            Command::SubscribeResourceEvents {
                terminal_id: pane.clone(),
                event_types: vec![ResourceEventType::CwdChanged],
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = subscribe(&mut a, 2, Some(pane.clone()), None).await;
        let (_, seen) = command(&mut a, 3, report_asked(&pane, "unfiltered-again")).await;
        let ids: Vec<&str> = seen.iter().filter_map(Seen::asked_id).collect();
        assert_eq!(
            ids,
            ["unfiltered-again"],
            "the latest subscription set the filter"
        );
        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_session_create_writes_seed_pane_names_the_writer() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut watcher, _) = connect(&socket, "watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;

        let (mut creator, _) = connect(&socket, "creator").await;
        send_frame(
            &mut creator,
            &FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: phux_protocol::wire::frame::SESSION_CREATE_KEY.to_owned(),
                value: br#"{"name":"made-by-creator"}"#.to_vec(),
            },
        )
        .await;
        let spawned = next_event(&mut watcher, |seen| {
            matches!(seen.event, AgentEvent::ResourceSpawned { .. })
        })
        .await;
        assert_eq!(
            spawned.actor_name(),
            Some("creator"),
            "the seed pane of a created session names the writer"
        );

        drop((watcher, creator));
        join_after_shutdown(shutdown, server).await;
    });
}

/// Every stamped event in `seen` has a higher `seq` than the one before it.
fn assert_strictly_increasing(seen: &[Seen]) {
    let seqs: Vec<u64> = seen
        .iter()
        .filter(|seen| seen.stamp.is_some())
        .map(Seen::seq)
        .collect();
    for pair in seqs.windows(2) {
        assert!(pair[0] < pair[1], "seq went {} then {}", pair[0], pair[1]);
    }
}

/// Read events until `done` accepts one, failing if the stream goes quiet
/// for the per-read deadline first.
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

#[test]
fn resume_after_a_thousand_missed_events_on_a_quiet_server_receives_them_all_in_order() {
    run_local(async {
        const MISSED: u32 = 1000;
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut producer, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut producer, "demo").await;
        for n in 0..MISSED {
            ask(&mut producer, 100 + n, &pane, &format!("missed-{n}")).await;
        }

        // The server is quiet from here on: nothing else is journaled.
        let (mut resumer, _) = connect(&socket, "resumer").await;
        send_frame(
            &mut resumer,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(0),
            },
        )
        .await;
        let last = format!("missed-{}", MISSED - 1);
        let seen = read_until(&mut resumer, |seen| seen.asked_id() == Some(last.as_str())).await;
        assert!(
            seen.iter()
                .all(|seen| !matches!(seen.event, AgentEvent::JournalGap { .. })),
            "the ring holds everything, so there is nothing to report missing"
        );
        let ids: Vec<&str> = seen.iter().filter_map(Seen::asked_id).collect();
        let expected: Vec<String> = (0..MISSED).map(|n| format!("missed-{n}")).collect();
        assert_eq!(ids, expected, "every missed event, in order");
        assert_strictly_increasing(&seen);

        drop((producer, resumer));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_stale_cursor_on_a_quiet_server_receives_its_gap_promptly() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |cfg| {
            cfg.event_journal_entries = 2;
        });
        let (mut producer, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut producer, "demo").await;
        for (n, id) in ["q1", "q2", "q3", "q4", "q5"].into_iter().enumerate() {
            ask(&mut producer, 10 + u32::try_from(n).unwrap(), &pane, id).await;
        }
        let (mut stale, _) = connect(&socket, "stale").await;
        send_frame(
            &mut stale,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(0),
            },
        )
        .await;
        let first = next_event(&mut stale, |_| true).await;
        assert!(
            matches!(
                first.event,
                AgentEvent::JournalGap {
                    first_missing: 1,
                    ..
                }
            ),
            "the first frame is the gap, with no later event needed: {first:?}"
        );
        drop((producer, stale));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_live_subscriber_that_stops_reading_receives_its_gap_when_it_reads_again() {
    run_local(async {
        const FLOOD: u32 = 400;
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut slow, _) = connect(&socket, "slow").await;
        let _ = subscribe(&mut slow, 1, None, None).await;
        let (mut flooder, _) = connect(&socket, "flooder").await;
        let pane = attach_pane(&mut flooder, "demo").await;
        let filler = "z".repeat(3000);
        for n in 0..FLOOD {
            let report = Command::ReportAsked {
                terminal_id: pane.clone(),
                id: format!("quiet-flood-{n}"),
                question: format!("{n} {filler}"),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            };
            let (result, _) = command(&mut flooder, 100 + n, report).await;
            assert_eq!(result, CommandResult::Ok, "the flooder is never blocked");
        }
        // Nothing more is journaled: the gap must arrive on its own.
        let seen = read_until(&mut slow, |seen| {
            matches!(seen.event, AgentEvent::JournalGap { .. })
        })
        .await;
        assert!(matches!(
            seen.last().map(|seen| &seen.event),
            Some(AgentEvent::JournalGap { .. })
        ));
        drop((slow, flooder));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_second_cursor_scope_on_one_connection_never_duplicates_or_reorders() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut producer, _) = connect(&socket, "producer").await;
        let pane = attach_pane(&mut producer, "demo").await;
        for (n, id) in ["d1", "d2", "d3"].into_iter().enumerate() {
            ask(&mut producer, 10 + u32::try_from(n).unwrap(), &pane, id).await;
        }
        let (mut both, _) = connect(&socket, "both-scopes").await;
        let mut seen = subscribe(&mut both, 1, None, Some(0)).await;
        seen.extend(subscribe(&mut both, 2, Some(pane.clone()), Some(0)).await);
        ask(&mut producer, 20, &pane, "d4").await;
        seen.extend(read_until(&mut both, |seen| seen.asked_id() == Some("d4")).await);
        let ids: Vec<&str> = seen.iter().filter_map(Seen::asked_id).collect();
        assert_eq!(ids, ["d1", "d2", "d3", "d4"], "each event once");
        assert_strictly_increasing(&seen);
        drop((producer, both));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_spawn_abandoned_mid_publication_still_journals_its_close() {
    run_local(async {
        const ROUNDS: u32 = 15;
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("demo"), |_| {});
        let (mut watcher, _) = connect(&socket, "watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;
        for n in 0..ROUNDS {
            // The spawner sends SPAWN_RESOURCE and vanishes at once, so its
            // publication fails wherever the disconnect lands.
            let (mut spawner, _) = connect(&socket, "abandoner").await;
            let _ = attach_pane(&mut spawner, "demo").await;
            send_frame(
                &mut spawner,
                &FrameKind::SpawnResource {
                    request_id: 700 + n,
                    group: GroupId::new(1),
                    command: Some(vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "sleep 600".to_owned(),
                    ]),
                    cwd: None,
                    env: None,
                    term: None,
                    satellite: None,
                    owner_terminal: None,
                    agent_session: None,
                    initial_size: None,
                    resource: None,
                },
            )
            .await;
            drop(spawner);
        }
        let seen = drain_until_quiet(&mut watcher, Duration::from_millis(1500)).await;
        let spawned: Vec<ResourceId> = seen
            .iter()
            .filter(|seen| matches!(seen.event, AgentEvent::ResourceSpawned { .. }))
            .filter_map(|seen| seen.terminal.clone())
            .collect();
        for (n, pane) in spawned.iter().enumerate() {
            let closes = seen
                .iter()
                .filter(|seen| {
                    seen.terminal.as_ref() == Some(pane)
                        && matches!(seen.event, AgentEvent::ResourceClosed { .. })
                })
                .count();
            let (alive, _) = command(
                &mut watcher,
                900 + u32::try_from(n).unwrap(),
                Command::GetTerminalState {
                    terminal_id: pane.clone(),
                    include_scrollback: false,
                    max_scrollback_lines: 0,
                },
            )
            .await;
            let expected = usize::from(matches!(alive, CommandResult::Error { .. }));
            assert_eq!(
                closes, expected,
                "{pane:?}: a pane that is gone has exactly one pane_closed, a live one none"
            );
        }
        drop(watcher);
        join_after_shutdown(shutdown, server).await;
    });
}
