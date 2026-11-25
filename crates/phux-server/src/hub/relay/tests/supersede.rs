//! A generation replaced without a tombstone, and the hub's own recovery of a
//! subscriber left with nothing to draw.

use super::*;

/// One generation of Terminal 7 under the satellite's own ids.
#[derive(Clone, Copy)]
struct Upstream(u64);

impl Upstream {
    fn ids(self) -> (StreamId, BootstrapId) {
        (
            StreamId::new(4).expect("stream"),
            BootstrapId::new(self.0).expect("bootstrap"),
        )
    }

    fn begin(self, base_seq: u64) -> FrameKind {
        let (stream_id, bootstrap_id) = self.ids();
        FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq,
        }
    }

    fn chunk(self, chunk_seq: u32) -> FrameKind {
        let (stream_id, bootstrap_id) = self.ids();
        FrameKind::BootstrapChunk {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: bytes::Bytes::from_static(b"screen"),
        }
    }

    fn ready(self) -> FrameKind {
        let (stream_id, bootstrap_id) = self.ids();
        FrameKind::BootstrapReady {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        }
    }

    fn output(self, seq: u64) -> FrameKind {
        let (stream_id, bootstrap_id) = self.ids();
        FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            seq,
            bytes: bytes::Bytes::from_static(b"live"),
        }
    }

    fn tombstone(self) -> FrameKind {
        let (stream_id, bootstrap_id) = self.ids();
        FrameKind::BootstrapTombstone {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            reason: TombstoneReason::Resize,
            last_valid_seq: 0,
        }
    }
}

fn session() -> RelaySession {
    negotiated(&[ServerFeature::ListDirectory])
}

/// Every frame a consumer was sent, as its kind and the generation it names.
fn received(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(&'static str, Option<u64>)> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .map(|message| {
            let Outbound::Frame(frame) = message else {
                panic!("unexpected sentinel");
            };
            let kind = match &frame {
                FrameKind::BootstrapBegin { .. } => "begin",
                FrameKind::BootstrapChunk { .. } => "chunk",
                FrameKind::BootstrapReady { .. } => "ready",
                FrameKind::BootstrapTombstone { .. } => "tombstone",
                FrameKind::ResourceOutput { .. } => "output",
                _ => "other",
            };
            (
                kind,
                stream_frame_generation(&frame).map(|(_, bootstrap)| bootstrap.get()),
            )
        })
        .collect()
}

/// A BEGIN over a live generation retires it downstream and opens a fresh
/// one, whatever id the satellite used: newer, older, or the very same one.
#[test]
fn a_begin_over_a_live_generation_supersedes_only_that_flow() {
    for replacement in [9, 1, 5] {
        let mut session = session();
        let (tx, mut rx) = mpsc::channel(32);
        attach(&mut session, 7, ClientId(1), tx);
        let first = Upstream(5);
        for frame in [
            first.begin(0),
            first.chunk(0),
            first.ready(),
            first.output(1),
        ] {
            session.feed(&frame);
        }
        let drawn = received(&mut rx)[0].1.expect("generation");

        let next = Upstream(replacement);
        for frame in [next.begin(1), next.chunk(0), next.ready(), next.output(2)] {
            session.feed(&frame);
        }
        let frames = received(&mut rx);
        assert_eq!(frames[0], ("tombstone", Some(drawn)));
        let fresh = frames[1].1.expect("generation");
        assert!(fresh > drawn, "consumers never see an id again");
        assert_eq!(
            frames[1..],
            ["begin", "chunk", "ready", "output"].map(|kind| (kind, Some(fresh)))
        );
    }
}

/// What the overlap check defended: two generations never interleave, a
/// superseded one is dead for good, and no budget leaks.
#[test]
fn a_superseded_generation_cannot_come_back() {
    let (first, next) = (Upstream(5), Upstream(6));
    for (label, late) in [
        ("chunk", first.chunk(1)),
        ("ready", first.ready()),
        ("output", first.output(1)),
        ("tombstone", first.tombstone()),
    ] {
        let mut session = session();
        for frame in [first.begin(0), first.chunk(0), next.begin(0)] {
            session.feed(&frame);
        }
        session
            .handle_inbound(&encode(&late))
            .expect_err("a superseded generation's frame must end the link");
        assert_eq!(session.bootstrap_flows.len(), 1, "{label}");
    }

    let mut session = session();
    for round in 1..=u64::from(MAX_RELAY_GENERATION_FRAMES) * 3 {
        session.feed(&Upstream(round).begin(0));
        session.feed(&Upstream(round).chunk(0));
    }
    assert_eq!(session.inflight_generation_frames, 2);
    assert_eq!(session.inflight_generation_bytes, 6);

    // Another Terminal's flow is untouched by any of it.
    let mut other = Upstream(1).begin(0);
    *stream_frame_scope_mut(&mut other).expect("scope") = ResourceId::local(8);
    session.feed(&other);
    session.feed(&Upstream(9999).begin(0));
    assert_eq!(
        session.bootstrap_flows[&8].bootstrap_id,
        Upstream(1).ids().1
    );
}

/// A tombstone that retires nothing is dropped; one that names another
/// generation than the live one still ends the link.
#[test]
fn a_repeated_tombstone_is_dropped() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), tx);
    let first = Upstream(5);
    for frame in [first.begin(0), first.tombstone(), first.tombstone()] {
        session.feed(&frame);
    }
    assert_eq!(received(&mut rx).len(), 2);
    session.feed(&Upstream(6).begin(0));
    session
        .handle_inbound(&encode(&first.tombstone()))
        .expect_err("a tombstone for a generation that is not live");
}

/// A viewer that joins mid-generation gets none of it until its own BEGIN.
#[test]
fn a_second_viewer_joining_mid_generation_starts_at_its_own_begin() {
    let mut session = session();
    let (tx_a, mut rx_a) = mpsc::channel(32);
    let (tx_b, mut rx_b) = mpsc::channel(32);
    attach(&mut session, 7, ClientId(1), tx_a);
    let (first, next) = (Upstream(5), Upstream(6));
    session.feed(&first.begin(0));
    attach(&mut session, 7, ClientId(2), tx_b);
    for frame in [first.chunk(0), first.ready(), first.output(1)] {
        session.feed(&frame);
    }
    assert_eq!(received(&mut rx_b), Vec::new());
    for frame in [next.begin(1), next.chunk(0), next.ready(), next.output(2)] {
        session.feed(&frame);
    }
    let kinds = |frames: Vec<(&'static str, Option<u64>)>| -> Vec<&'static str> {
        frames.into_iter().map(|(kind, _)| kind).collect()
    };
    assert_eq!(
        kinds(received(&mut rx_b)),
        ["begin", "chunk", "ready", "output"]
    );
    assert_eq!(
        received(&mut rx_a).len(),
        9,
        "A saw both, and the tombstone"
    );
}

/// An ack names the hub's id downstream and the satellite's upstream; one for
/// a generation no longer live goes nowhere.
#[test]
fn a_frame_ack_reaches_the_satellite_under_its_own_id() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), tx);
    session.feed(&Upstream(5).begin(0));
    let outward = received(&mut rx)[0].1.expect("generation");
    let ack = |bootstrap: u64| FrameKind::FrameAck {
        terminal_id: ResourceId::local(7),
        stream_id: Upstream(5).ids().0,
        bootstrap_id: BootstrapId::new(bootstrap).expect("bootstrap"),
        seq: 1,
    };
    let forward = |session: &mut RelaySession, frame| {
        session.handle_request_checked(RelayRequest::Forward { frame })
    };
    let wire = forward(&mut session, ack(outward)).expect("live ack");
    assert_eq!(decode(&wire), ack(5));
    assert!(forward(&mut session, ack(5)).is_none());
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

/// A subscriber that overflowed is fenced, kept, and bootstrapped again.
#[test]
fn an_overflowed_viewer_is_fenced_and_bootstrapped_again() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(1);
    attach(&mut session, 7, ClientId(1), tx);
    let first = Upstream(5);
    session.feed(&first.begin(0));
    for chunk_seq in 0..=u32::try_from(MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES).expect("small") {
        session.feed(&first.chunk(chunk_seq));
    }
    assert!(matches!(
        session.subscribers[&7][0].gate,
        SnapshotGate::Retiring { .. }
    ));
    assert_eq!(received(&mut rx).len(), 1, "only BEGIN fit");
    session.flush_pending_snapshots();
    assert_eq!(received(&mut rx)[0].0, "tombstone");
    assert!(session.take_delivery_detaches().is_empty(), "kept");
    assert!(session.take_bootstrap_asks().is_empty(), "not at once");

    std::thread::sleep(std::time::Duration::from_millis(300));
    let asks = session.take_bootstrap_asks();
    assert_eq!(asks.len(), 1);
    assert!(matches!(
        decode(&asks[0]),
        FrameKind::Command { command: Command::AttachResource { terminal_id, .. }, .. }
            if terminal_id == ResourceId::local(7)
    ));
    session.feed(&Upstream(6).begin(0));
    assert_eq!(received(&mut rx)[0].0, "begin");
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
