//! A generation replaced without a tombstone.

use super::*;

/// One generation of Terminal 7 under the satellite's own ids.
#[derive(Clone, Copy)]
pub(super) struct Upstream(pub(super) u64);

impl Upstream {
    pub(super) fn ids(self) -> (StreamId, BootstrapId) {
        (
            StreamId::new(4).expect("stream"),
            BootstrapId::new(self.0).expect("bootstrap"),
        )
    }

    pub(super) fn begin(self, base_seq: u64) -> FrameKind {
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

    pub(super) fn chunk(self, chunk_seq: u32) -> FrameKind {
        let (stream_id, bootstrap_id) = self.ids();
        FrameKind::BootstrapChunk {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: bytes::Bytes::from_static(b"screen"),
        }
    }

    pub(super) fn ready(self) -> FrameKind {
        let (stream_id, bootstrap_id) = self.ids();
        FrameKind::BootstrapReady {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        }
    }

    pub(super) fn output(self, seq: u64) -> FrameKind {
        let (stream_id, bootstrap_id) = self.ids();
        FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            seq,
            bytes: bytes::Bytes::from_static(b"live"),
        }
    }

    pub(super) fn tombstone(self) -> FrameKind {
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

pub(super) fn session() -> RelaySession {
    negotiated(&[ServerFeature::ListDirectory])
}

/// Every frame a consumer was sent, as its kind and the generation it names.
pub(super) fn received(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(&'static str, Option<u64>)> {
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
