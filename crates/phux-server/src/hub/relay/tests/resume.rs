//! The hub's recovery of a consumer left with nothing to draw.

use super::*;
use std::time::{Duration, Instant};

/// Another Terminal's frame, from Terminal 7's.
fn on(terminal: u32, mut frame: FrameKind) -> FrameKind {
    *stream_frame_scope_mut(&mut frame).expect("scope") = ResourceId::local(terminal);
    frame
}

/// Answer every command in flight.
fn answer(session: &mut RelaySession, result: &CommandResult) {
    let mut in_flight: Vec<u32> = session.pending.keys().copied().collect();
    in_flight.sort_unstable();
    for request_id in in_flight {
        session.feed(&FrameKind::CommandResult {
            request_id,
            result: result.clone(),
        });
    }
}

fn refusal(code: ErrorCode) -> CommandResult {
    CommandResult::error(code, "refused by the satellite")
}

/// Terminal 7's only viewer.
fn viewer(session: &mut RelaySession) -> &mut ProxySubscriber {
    &mut session.subscribers.get_mut(&7).expect("viewers")[0]
}

/// Make the viewer's next ask due, and send it.
fn ask_now(session: &mut RelaySession) -> usize {
    let content = viewer(session).content.as_mut().expect("content");
    assert!(content.ask_at.is_some(), "no bootstrap is owed");
    content.ask_at = Some(Instant::now());
    session.take_bootstrap_asks().len()
}

fn frames(rx: &mut mpsc::Receiver<Outbound>) -> Vec<FrameKind> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .map(|message| {
            let Outbound::Frame(frame) = message else {
                panic!("unexpected sentinel");
            };
            frame
        })
        .collect()
}

/// A failed subscription ends in one error, and upstream in a detach.
fn assert_failed(session: &mut RelaySession, rx: &mut mpsc::Receiver<Outbound>, code: ErrorCode) {
    session.flush_pending_snapshots();
    assert!(!session.subscribers.contains_key(&7), "still subscribed");
    assert!(
        matches!(frames(rx).as_slice(), [FrameKind::Error { request_id: None, code: got, .. }] if *got == code)
    );
    assert_eq!(session.take_delivery_detaches().len(), 1);
}

/// A consumer whose other producers always wait for a slot still gets its
/// bootstrap: a bare `try_send` loses every slot to them.
#[tokio::test]
async fn waiting_producers_do_not_starve_a_retained_bootstrap() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(1);
    attach(&mut session, 7, ClientId(1), tx.clone());
    let busy = |tx: mpsc::Sender<Outbound>| async move {
        while tx.send(Outbound::Frame(FrameKind::Detach)).await.is_ok() {}
    };
    let producers = [tokio::spawn(busy(tx.clone())), tokio::spawn(busy(tx))];
    while rx.is_empty() {
        tokio::task::yield_now().await;
    }
    let first = Upstream(5);
    for frame in [first.begin(0), first.chunk(0), first.ready()] {
        session.feed(&frame);
    }
    let mut relayed = 0;
    for _ in 0..64 {
        if !matches!(rx.recv().await, Some(Outbound::Frame(FrameKind::Detach))) {
            relayed += 1;
        }
        tokio::task::yield_now().await;
        session.flush_pending_snapshots();
        tokio::task::yield_now().await;
    }
    assert_eq!(relayed, 3, "BEGIN, CHUNK and READY each took their turn");
    for producer in producers {
        producer.abort();
    }
}

/// Content subscribers cross a redial of the same satellite process, less the
/// ones that left or whose attach was refused; a restarted satellite's ids
/// name other panes, so nothing crosses to it.
#[test]
fn only_live_attaches_of_the_same_satellite_process_resume() {
    let lost = || {
        let mut session = session();
        session.set_incarnation(Some([1; 16]));
        let mut receivers = Vec::new();
        for client in 1..=4 {
            let (tx, rx) = mpsc::channel(8);
            attach(&mut session, 7, ClientId(client), tx);
            receivers.push(rx);
        }
        // Answered: every attach but client 4's, still in flight.
        let mut answered: Vec<u32> = session.pending.keys().copied().collect();
        answered.sort_unstable();
        for request_id in answered.into_iter().take(3) {
            session.feed(&FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            });
        }
        let (tx, rx) = mpsc::channel(8);
        subscribe(&mut session, 7, ClientId(5), tx);
        receivers.push(rx);
        let mut resume = session.teardown("lost");
        resume.withdraw(&Unsubscribe::Client(ClientId(1)));
        resume.withdraw(&Unsubscribe::Terminal {
            client: ClientId(2),
            terminal: 7,
            seq: 1,
            reply: None,
        });
        (resume, receivers)
    };

    let (resume, _receivers) = lost();
    let mut next = session();
    next.set_incarnation(Some([1; 16]));
    assert!(!resume.satellite_restarted(next.incarnation()));
    next.resume(resume);
    let held: Vec<_> = next.subscribers[&7].iter().map(|sub| sub.client).collect();
    assert_eq!(
        held,
        [ClientId(2), ClientId(3)],
        "a stale detach is ignored"
    );
    assert_eq!(
        next.take_bootstrap_asks().len(),
        1,
        "one attach serves both"
    );

    let (resume, _receivers) = lost();
    let mut restarted = session();
    restarted.set_incarnation(Some([2; 16]));
    assert!(resume.satellite_restarted(restarted.incarnation()));
    restarted.resume(resume);
    assert!(restarted.subscribers.is_empty());
    assert!(restarted.take_bootstrap_asks().is_empty());
}

/// Panes of a lost satellite hold no mailbox slot while the link is down: the
/// consumer's local output, replies and pongs need them.
#[test]
fn panes_of_a_lost_satellite_hold_no_mailbox_slots() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(8);
    for terminal in 1..=8 {
        attach(&mut session, terminal, ClientId(1), tx.clone());
    }
    for _ in 0..8 {
        tx.try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill");
    }
    for terminal in 1..=8 {
        session.feed(&on(terminal, Upstream(1).begin(0)));
    }
    answer(&mut session, &CommandResult::Ok);
    let resume = session.teardown("lost");
    assert_eq!(frames(&mut rx).len(), 8);
    assert!(
        tx.try_reserve_many(8).is_ok(),
        "the lost panes hold the consumer's slots"
    );
    drop(resume);
}

/// A refused fresh attach ends its viewer's subscription with the refusal; a
/// terminal that is gone closes; silence is an error too.
#[test]
fn a_refused_or_unanswered_ask_is_told_to_the_viewer() {
    let asked = || {
        let mut session = session();
        session.set_incarnation(Some([1; 16]));
        let (tx, rx) = mpsc::channel(8);
        attach(&mut session, 7, ClientId(1), tx);
        answer(&mut session, &CommandResult::Ok);
        assert_eq!(ask_now(&mut session), 1);
        assert!(session.take_bootstrap_asks().is_empty(), "one at a time");
        (session, rx)
    };

    let (mut session, mut rx) = asked();
    answer(&mut session, &refusal(ErrorCode::PermissionDenied));
    assert!(session.take_bootstrap_asks().is_empty());
    assert_failed(&mut session, &mut rx, ErrorCode::PermissionDenied);

    let (mut session, mut rx) = asked();
    session.bootstrap_asks[0].deadline = Instant::now();
    assert!(session.take_bootstrap_asks().is_empty());
    assert_failed(&mut session, &mut rx, ErrorCode::SatelliteUnreachable);

    let (mut session, mut rx) = asked();
    answer(&mut session, &refusal(ErrorCode::TerminalNotFound));
    assert!(session.take_bootstrap_asks().is_empty());
    assert!(matches!(
        frames(&mut rx).as_slice(),
        [FrameKind::ResourceClosed { .. }]
    ));
    assert!(!session.subscribers.contains_key(&7));
}

/// Attaches answered without a generation are repeated a bounded number of
/// times, then told. One whose bootstrap is on its way, or that is not
/// answered yet, is left alone however long it takes.
#[test]
fn an_attach_answered_without_a_screen_is_repeated_then_fails() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), tx);
    assert!(
        viewer(&mut session)
            .content
            .as_ref()
            .expect("content")
            .ask_at
            .is_none(),
        "nothing is owed before the answer"
    );
    session.feed(&Upstream(1).begin(0));
    session.feed(&Upstream(1).chunk(0));
    answer(&mut session, &CommandResult::Ok);
    assert!(
        viewer(&mut session)
            .content
            .as_ref()
            .expect("content")
            .ask_at
            .is_none(),
        "a generation is on its way"
    );
    session.feed(&Upstream(1).tombstone());
    let _ = frames(&mut rx);

    session.attach_answered(7);
    for _ in 0..MAX_BOOTSTRAP_ASKS {
        assert_eq!(ask_now(&mut session), 1);
        answer(&mut session, &CommandResult::Ok);
        let _ = session.take_bootstrap_asks();
    }
    assert_failed(&mut session, &mut rx, ErrorCode::InternalError);
}

/// Overflow the viewer's delivery once: fenced, with its tombstone delivered.
fn overflow(session: &mut RelaySession, rx: &mut mpsc::Receiver<Outbound>, generation: u64) {
    let upstream = Upstream(generation);
    session.feed(&upstream.begin(0));
    for chunk_seq in 0..=u32::try_from(MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES).expect("small") {
        session.feed(&upstream.chunk(chunk_seq));
    }
    session.feed(&upstream.ready());
    let _ = frames(rx);
    session.flush_pending_snapshots();
    let kinds: Vec<_> = received(rx).into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, ["tombstone"]);
}

/// A viewer that overflowed is fenced, kept and bootstrapped again; one that
/// keeps overflowing recuts the pane for everyone, so it is failed instead.
#[test]
fn an_overflowing_viewer_is_bootstrapped_again_but_not_forever() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(1);
    attach(&mut session, 7, ClientId(1), tx);
    answer(&mut session, &CommandResult::Ok);
    let mut waits = Vec::new();
    for generation in 1..=u64::from(MAX_BOOTSTRAP_ASKS) {
        overflow(&mut session, &mut rx, generation * 2);
        assert!(session.take_delivery_detaches().is_empty(), "kept");
        assert!(session.take_bootstrap_asks().is_empty(), "not at once");
        let content = viewer(&mut session).content.as_ref().expect("content");
        waits.push(content.ask_at.expect("owed") - Instant::now());
        assert_eq!(ask_now(&mut session), 1);

        // The fresh one is delivered whole.
        let fresh = Upstream(generation * 2 + 1);
        for frame in [fresh.begin(0), fresh.ready()] {
            session.feed(&frame);
            assert_eq!(received(&mut rx).len(), 1);
        }
        answer(&mut session, &CommandResult::Ok);
        let _ = session.take_bootstrap_asks();
        assert!(matches!(viewer(&mut session).gate, SnapshotGate::Open));
    }
    assert!(
        waits.windows(2).all(|pair| pair[1] > pair[0] * 2),
        "{waits:?}"
    );
    assert!(waits[0] < Duration::from_millis(300), "the first is soon");

    overflow(&mut session, &mut rx, 99);
    assert_failed(&mut session, &mut rx, ErrorCode::ResourceExhausted);
}

/// A consumer whose first content attach failed keeps only its events: told
/// it is not attached, it is bootstrapped by nothing, a redial included.
#[test]
fn a_failed_first_attach_leaves_an_event_subscriber() {
    let upgraded = || {
        let mut session = session();
        session.set_incarnation(Some([1; 16]));
        let (tx, rx) = mpsc::channel(8);
        subscribe(&mut session, 7, ClientId(1), tx.clone());
        attach(&mut session, 7, ClientId(1), tx);
        (session, rx)
    };
    let (mut refused, _rx) = upgraded();
    answer(&mut refused, &refusal(ErrorCode::PermissionDenied));
    // `attach` dropped its reply receiver, as a consumer that gave up does.
    let (mut abandoned, _rx) = upgraded();
    assert_eq!(abandoned.prune_abandoned(), 1);
    for mut session in [refused, abandoned] {
        assert!(viewer(&mut session).content.is_none());
        let mut next = super::supersede::session();
        next.set_incarnation(Some([1; 16]));
        next.resume(session.teardown("lost"));
        assert!(next.subscribers.is_empty());
    }
}

/// An abandoned first attach is withdrawn, and detached upstream in case the
/// satellite served it late.
#[test]
fn an_abandoned_attach_is_withdrawn_and_detached() {
    let mut session = session();
    let (tx, _rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), tx);
    assert_eq!(session.prune_abandoned(), 1);
    assert!(session.subscribers.is_empty());
    assert_eq!(session.take_delivery_detaches().len(), 1);
}

/// A generation that replaces retained output opens the gate on its own READY,
/// not on what the gate was when the output was retained.
#[test]
fn a_replacement_behind_retained_output_waits_for_its_ready() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(2);
    attach(&mut session, 7, ClientId(1), tx);
    let (first, next) = (Upstream(1), Upstream(2));
    for frame in [first.begin(0), first.ready(), first.output(1)] {
        session.feed(&frame);
    }
    session.feed(&next.begin(1));
    let _ = frames(&mut rx);
    session.flush_pending_snapshots();
    assert_eq!(
        received(&mut rx).first().map(|frame| frame.0),
        Some("begin")
    );
    assert!(matches!(
        viewer(&mut session).gate,
        SnapshotGate::AwaitingFirst
    ));
    session.feed(&next.ready());
    assert!(matches!(viewer(&mut session).gate, SnapshotGate::Open));
}
