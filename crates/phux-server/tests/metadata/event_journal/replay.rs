//! Sequence, cursor replay, and gap delivery.

use std::time::Duration;

use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, FrameKind};

use phux_server_testkit::{run_local, send_frame};

use super::{
    Seen, Server, ask, ask_padded, asked_ids, assert_strictly_increasing, command,
    drain_until_quiet, get_state, journal_contents, journal_head, next_event, read_until,
    report_asked, spawn_pane, subscribe,
};
use crate::common::{connect_as, full_caps};

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
