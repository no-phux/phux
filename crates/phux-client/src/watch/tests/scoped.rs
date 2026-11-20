use std::time::Duration;

use crate::agent_meta::AgentMetaState;
use crate::resource::cursor::ResumeState;
use crate::testkit::{EndOfScript, ScriptSpec, serve_one};
use crate::watch::{WatchItem, WatchOutcome, watch_events, watch_resumable};
use phux_protocol::wire::frame::AgentEvent;

use super::{FrameKind, RESOURCE_AGENT_KEY, ResourceId, Scope, agent_record};

/// Drive `watch_events` against the shared scripted server, collecting
/// every streamed item and returning it alongside the frames the client
/// actually sent.
async fn drive(
    terminal: Option<ResourceId>,
    script: Vec<FrameKind>,
) -> (Vec<WatchItem>, Vec<FrameKind>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let (socket, server) = serve_one(
        dir.path(),
        ScriptSpec::new().extend(script).end(EndOfScript::HangUp),
    );
    let mut items = Vec::new();
    watch_events(&socket, terminal, |item| {
        items.push(item);
        true
    })
    .await
    .expect("a scripted hang-up is a clean EOF, not an error");
    let seen = server.await.expect("scripted server task");
    (items, seen)
}

/// A terminal-scoped watch asks for events and the `phux.agent/v1` key,
/// and never attaches.
#[tokio::test]
async fn terminal_scoped_watch_subscribes_to_events_and_the_agent_key() {
    let pane = ResourceId::local(7);
    let (_items, seen) = drive(
        Some(pane.clone()),
        vec![agent_record(&pane, r#"{"name":"reviewer"}"#)],
    )
    .await;

    assert!(
        seen.iter().any(|f| matches!(
            f,
            FrameKind::SubscribeEvents { terminal: Some(id), .. } if *id == pane
        )),
        "watch must subscribe to events for the pane; sent {seen:?}"
    );
    assert!(
        seen.iter().any(|f| matches!(
            f,
            FrameKind::SubscribeMetadata { scope: Scope::Resource(id), key }
                if *id == pane && key == RESOURCE_AGENT_KEY
        )),
        "watch must subscribe to the pane's phux.agent/v1 record; sent {seen:?}"
    );
    assert!(
        !seen.iter().any(|f| matches!(
            f,
            FrameKind::Attach { .. } | FrameKind::ViewportResize { .. }
        )),
        "watch must not attach or report a viewport; sent {seen:?}"
    );
}

#[tokio::test]
async fn agent_records_stream_as_agent_state_items_carrying_the_previous_record() {
    let pane = ResourceId::local(7);
    let (items, _seen) = drive(
        Some(pane.clone()),
        vec![
            agent_record(
                &pane,
                r#"{"name":"reviewer","kind":"claude","state":"working"}"#,
            ),
            agent_record(
                &pane,
                r#"{"name":"reviewer","kind":"claude","state":"blocked"}"#,
            ),
        ],
    )
    .await;

    assert_eq!(items.len(), 2, "one item per record: {items:?}");
    let WatchItem::AgentState(first) = &items[0] else {
        panic!("expected an agent-state item, got {:?}", items[0]);
    };
    assert_eq!(first.terminal.as_ref(), Some(&pane));
    assert_eq!(
        first.record.as_ref().unwrap().state,
        AgentMetaState::Working
    );
    assert!(
        first.previous.is_none(),
        "the first record for a Terminal has nothing to transition from"
    );

    let WatchItem::AgentState(second) = &items[1] else {
        panic!("expected an agent-state item, got {:?}", items[1]);
    };
    assert_eq!(
        second.record.as_ref().unwrap().state,
        AgentMetaState::Blocked
    );
    assert_eq!(
        second.previous.as_ref().unwrap().state,
        AgentMetaState::Working,
        "the transition the consumer is waiting on"
    );
}

/// A tombstone and an unreadable value both stream as a cleared record
/// (L3 §3.7), other keys are ignored, and events interleave in push order.
#[tokio::test]
async fn clears_other_keys_and_ordering_on_one_stream() {
    let pane = ResourceId::local(3);
    let (items, _seen) = drive(
        Some(pane.clone()),
        vec![
            agent_record(&pane, r#"{"name":"reviewer","state":"blocked"}"#),
            FrameKind::MetadataChanged {
                scope: Scope::Resource(pane.clone()),
                key: "phux.tui.layout/v1".to_owned(),
                value: Some(b"{}".to_vec()),
                actor: None,
            },
            FrameKind::Event {
                terminal: Some(pane.clone()),
                event: AgentEvent::Bell,
                stamp: None,
            },
            FrameKind::MetadataChanged {
                scope: Scope::Resource(pane.clone()),
                key: RESOURCE_AGENT_KEY.to_owned(),
                value: None,
                actor: None,
            },
            agent_record(&pane, "not a record"),
        ],
    )
    .await;

    assert_eq!(items.len(), 4, "the foreign key is ignored: {items:?}");
    assert!(matches!(items[1], WatchItem::Event(_)));
    let WatchItem::AgentState(tombstone) = &items[2] else {
        panic!("expected an agent-state item, got {:?}", items[2]);
    };
    assert!(tombstone.record.is_none());
    assert_eq!(
        tombstone.previous.as_ref().unwrap().state,
        AgentMetaState::Blocked
    );
    let WatchItem::AgentState(unreadable) = &items[3] else {
        panic!("expected an agent-state item, got {:?}", items[3]);
    };
    assert!(unreadable.record.is_none());
}

// -- bounded gate ------------------------------------------------------

/// Drive [`watch_resumable`] (no cursor) against the scripted server,
/// stopping on the first item `accept` takes.
async fn drive_bounded<P>(
    script: Vec<FrameKind>,
    end: EndOfScript,
    timeout: Option<Duration>,
    mut accept: P,
) -> (WatchOutcome, Vec<WatchItem>)
where
    P: FnMut(&WatchItem) -> bool,
{
    let dir = tempfile::tempdir().expect("temp dir");
    let (socket, server) = serve_one(dir.path(), ScriptSpec::new().extend(script).end(end));
    let mut items = Vec::new();
    let mut resume = ResumeState::new(None);
    let outcome = watch_resumable(
        &socket,
        ResourceId::local(7),
        &mut resume,
        timeout,
        |item| {
            let stop = accept(&item);
            items.push(item);
            !stop
        },
    )
    .await
    .expect("scripted transport");
    server.await.expect("scripted server task");
    (outcome, items)
}

fn event(event: AgentEvent) -> FrameKind {
    FrameKind::Event {
        terminal: Some(ResourceId::local(7)),
        event,
        stamp: None,
    }
}

fn is_bell(item: &WatchItem) -> bool {
    matches!(item, WatchItem::Event(ev) if ev.event == AgentEvent::Bell)
}

/// `phux watch --until`: the matching item is delivered before the stream
/// stops; an agent-state item can satisfy the gate too.
#[tokio::test]
async fn a_bounded_watch_stops_on_the_first_matching_item_and_delivers_it() {
    let (outcome, items) = drive_bounded(
        vec![
            event(AgentEvent::Dirty),
            event(AgentEvent::Bell),
            event(AgentEvent::Idle),
        ],
        EndOfScript::HangUp,
        None,
        is_bell,
    )
    .await;
    assert_eq!(outcome, WatchOutcome::Stopped);
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(is_bell(&items[1]));

    let pane = ResourceId::local(7);
    let (outcome, items) = drive_bounded(
        vec![agent_record(
            &pane,
            r#"{"name":"reviewer","state":"blocked"}"#,
        )],
        EndOfScript::HangUp,
        None,
        |item| matches!(item, WatchItem::AgentState(_)),
    )
    .await;

    assert_eq!(outcome, WatchOutcome::Stopped);
    assert_eq!(items.len(), 1);
}

/// A silent connected server ends in `TimedOut` (the prefix still
/// delivered); a server EOF before the match is `Ended`, never a
/// satisfied gate.
#[tokio::test]
async fn a_bounded_watch_tells_a_deadline_from_a_server_eof() {
    let (outcome, items) = drive_bounded(
        vec![event(AgentEvent::Dirty)],
        EndOfScript::ServeUntilDetach,
        Some(Duration::from_millis(150)),
        is_bell,
    )
    .await;
    assert_eq!(outcome, WatchOutcome::TimedOut);
    assert_eq!(items.len(), 1, "{items:?}");

    let (outcome, _items) = drive_bounded(
        vec![event(AgentEvent::Dirty)],
        EndOfScript::HangUp,
        Some(Duration::from_secs(30)),
        is_bell,
    )
    .await;
    assert_eq!(outcome, WatchOutcome::Ended);
}
