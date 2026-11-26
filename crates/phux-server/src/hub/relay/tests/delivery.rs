//! A consumer's mailbox: turns, reservations, and what waits behind it.

use super::resume::{answer, frames, on, viewer};
use super::*;

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
