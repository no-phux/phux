use std::time::Duration;

use crate::resource::cursor::ResumeState;
use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer, serve_one};
use crate::watch::{WatchItem, WatchOutcome, watch_live_async, watch_resumable};
use phux_protocol::ids::{SessionId, WindowId};
use phux_protocol::wire::frame::{AgentEvent, EventStamp};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::{FrameKind, ResourceId, snapshot};

// -- watch_resumable --------------------------------------------------

#[tokio::test]
async fn live_reactions_refuse_a_vanished_pane_or_missing_journal_before_subscribing() {
    let target = ResourceId::local(7);
    let cases = [
        (
            snapshot(Vec::new()).with_journal_head(Some(40)),
            "not a live pane",
        ),
        (
            snapshot(vec![ResourceInfo::new(
                target.clone(),
                WindowId::new(1),
                80,
                24,
            )]),
            "require a server event journal",
        ),
    ];
    for (state, message) in cases {
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = serve_one(dir.path(), ScriptSpec::new().state(state));
        let result = watch_live_async(&socket, target.clone(), async |_| true).await;
        assert!(result.unwrap_err().to_string().contains(message));
        let seen = server.await.expect("scripted server");
        assert!(
            !seen
                .iter()
                .any(|frame| matches!(frame, FrameKind::SubscribeEvents { .. }))
        );
    }
}

fn stamped(pane: &ResourceId, event: AgentEvent, seq: u64) -> FrameKind {
    FrameKind::Event {
        terminal: Some(pane.clone()),
        event,
        stamp: Some(Box::new(EventStamp::new(seq, 1_000 + seq))),
    }
}

async fn drive_resumable(
    resume: &mut ResumeState,
    script: Vec<FrameKind>,
) -> (WatchOutcome, Vec<WatchItem>, Vec<FrameKind>) {
    drive_resumable_with(resume, script, ScriptSpec::new()).await
}

async fn drive_resumable_with(
    resume: &mut ResumeState,
    script: Vec<FrameKind>,
    spec: ScriptSpec,
) -> (WatchOutcome, Vec<WatchItem>, Vec<FrameKind>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let spec = spec
        .server_id(vec![0x01, 0x02])
        .extend(script)
        .end(EndOfScript::HangUp);
    let (socket, server) = serve_one(dir.path(), spec);
    let mut items = Vec::new();
    let outcome = watch_resumable(&socket, ResourceId::local(7), resume, None, |item| {
        items.push(item);
        true
    })
    .await
    .expect("scripted transport");
    let seen = server.await.expect("scripted server task");
    (outcome, items, seen)
}

/// The cursor goes in as `after_seq`, every stamped event carries its
/// `seq` to the item, and the reached position comes back out.
#[tokio::test]
async fn a_resumable_watch_replays_from_its_cursor_and_advances_it() {
    use crate::resource::cursor::Cursor;

    let pane = ResourceId::local(7);
    let mut resume = ResumeState::new(Cursor::new(vec![0x01, 0x02], 5));
    let (outcome, items, seen) = drive_resumable(
        &mut resume,
        vec![
            stamped(&pane, AgentEvent::Bell, 6),
            stamped(&pane, AgentEvent::Dirty, 9),
        ],
    )
    .await;
    assert_eq!(outcome, WatchOutcome::Ended);
    assert!(seen.iter().any(|frame| matches!(
        frame,
        FrameKind::SubscribeEvents {
            after_seq: Some(5),
            ..
        }
    )));
    assert_eq!(items.len(), 2);
    let WatchItem::Event(first) = &items[0] else {
        panic!("expected an event, got {:?}", items[0]);
    };
    assert_eq!(first.stamp.as_ref().map(|stamp| stamp.seq), Some(6));
    assert!(!resume.cursor_void());
    assert_eq!(resume.cursor().expect("cursor").to_string(), "0102:9");
}

/// A cursor from another incarnation is not sent; the watch starts live
/// and says so.
#[tokio::test]
async fn a_resumable_watch_voids_a_foreign_cursor() {
    use crate::resource::cursor::Cursor;

    let mut resume = ResumeState::new(Cursor::new(vec![0xff], 5));
    let (_outcome, _items, seen) = drive_resumable(&mut resume, Vec::new()).await;
    // Not sent; the watch starts with journal semantics and no replay, so
    // an `expired` lease reads as `expired` rather than `released`.
    assert!(seen.iter().any(|frame| matches!(
        frame,
        FrameKind::SubscribeEvents {
            after_seq: Some(crate::resource::cursor::NO_REPLAY),
            ..
        }
    )));
    assert!(resume.cursor_void());
    assert_eq!(resume.cursor().expect("fresh cursor").to_string(), "0102:0");
}

/// A live start is anchored at the journal head: the subscription
/// replays from it, and a run that saw no event reports the head as its
/// cursor, not `SERVER_ID:0` (whose resume replays the whole journal).
#[tokio::test]
async fn a_live_watch_reports_the_journal_head_when_it_saw_nothing() {
    let state = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
        .with_journal_head(Some(30));
    let mut resume = ResumeState::new(None);
    let (_outcome, items, seen) =
        drive_resumable_with(&mut resume, Vec::new(), ScriptSpec::new().state(state)).await;
    assert!(items.is_empty(), "{items:?}");
    assert!(
        seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SubscribeEvents {
                after_seq: Some(30),
                ..
            }
        )),
        "replays from the head: {seen:?}"
    );
    assert_eq!(resume.cursor().expect("cursor").to_string(), "0102:30");
}

/// phux-l3ox: a zero timeout still subscribes once and reports where to
/// resume, however slowly the server answers inside the setup floor.
/// Before the floor the deadline also covered the connect, so a server
/// that took longer than the timer's first tick to say `HELLO_OK` left
/// the watch with no cursor at all.
#[tokio::test]
async fn a_zero_timeout_still_anchors_a_cursor_from_a_slow_server() {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("slow.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
    let state = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
        .with_journal_head(Some(30));
    let spec = ScriptSpec::new()
        .state(state)
        .server_id(vec![0x01, 0x02])
        .end(EndOfScript::ServeUntilDetach);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        // Far longer than a zero deadline's first tick; well inside the floor.
        tokio::time::sleep(Duration::from_millis(200)).await;
        ScriptedServer::on_stream(stream, spec).run().await
    });
    let mut resume = ResumeState::new(None);
    let outcome = watch_resumable(
        &socket,
        ResourceId::local(7),
        &mut resume,
        Some(Duration::ZERO),
        |_| true,
    )
    .await
    .expect("scripted transport");
    assert_eq!(outcome, WatchOutcome::TimedOut);
    assert_eq!(resume.cursor().expect("cursor").to_string(), "0102:30");
    let seen = server.await.expect("scripted server task");
    assert!(
        seen.iter()
            .any(|frame| matches!(frame, FrameKind::SubscribeEvents { .. })),
        "the zero-timeout watch subscribed: {seen:?}"
    );
}

/// The floor is a floor, not a hang: a server that never answers `HELLO`
/// still ends a zero-timeout watch, at the floor.
#[tokio::test(start_paused = true)]
async fn a_zero_timeout_against_a_silent_server_ends_at_the_floor() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (socket, peer) = crate::testkit::spawn_hold_silent(dir.path());
    let started = tokio::time::Instant::now();
    let mut resume = ResumeState::new(None);
    let outcome = watch_resumable(
        &socket,
        ResourceId::local(7),
        &mut resume,
        Some(Duration::ZERO),
        |_| true,
    )
    .await
    .expect("a silent peer is a timeout, not an error");
    assert_eq!(outcome, WatchOutcome::TimedOut);
    assert_eq!(started.elapsed(), crate::wait::FIRST_READ_FLOOR);
    assert!(resume.cursor().is_none());
    peer.abort();
}
