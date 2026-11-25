//! The hub's recovery of a consumer left with nothing to draw.

use super::*;
use std::time::{Duration, Instant};

/// Another Terminal's frame, from Terminal 7's.
pub(super) fn on(terminal: u32, mut frame: FrameKind) -> FrameKind {
    *stream_frame_scope_mut(&mut frame).expect("scope") = ResourceId::local(terminal);
    frame
}

/// Answer every command in flight.
pub(super) fn answer(session: &mut RelaySession, result: &CommandResult) {
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
pub(super) fn viewer(session: &mut RelaySession) -> &mut ProxySubscriber {
    &mut session.subscribers.get_mut(&7).expect("viewers")[0]
}

/// Reach the viewer's deadline; how many asks that sent.
fn at_deadline(session: &mut RelaySession) -> usize {
    let content = viewer(session).content.as_mut().expect("content");
    assert!(content.deadline.is_some(), "undrawn without a deadline");
    content.deadline = Some(Instant::now());
    session.settle_undrawn().len()
}

/// Time left to the viewer's deadline.
fn wait_left(session: &mut RelaySession) -> Duration {
    let content = viewer(session).content.as_ref().expect("content");
    content.deadline.expect("a deadline") - Instant::now()
}

pub(super) fn frames(rx: &mut mpsc::Receiver<Outbound>) -> Vec<FrameKind> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .map(|message| {
            let Outbound::Frame(frame) = message else {
                panic!("unexpected sentinel");
            };
            frame
        })
        .collect()
}

/// A failed subscription ends in the hub's own error and an upstream detach.
fn assert_failed(session: &mut RelaySession, rx: &mut mpsc::Receiver<Outbound>) {
    session.flush_pending_snapshots();
    assert!(!session.subscribers.contains_key(&7), "still subscribed");
    let told = frames(rx);
    let [
        FrameKind::Error {
            request_id: None,
            code,
            message,
        },
    ] = told.as_slice()
    else {
        panic!("{told:?}");
    };
    assert_eq!(
        code.scope(),
        phux_protocol::wire::frame::ErrorScope::Terminal
    );
    assert_ne!(*code, ErrorCode::SatelliteUnreachable, "greys a whole host");
    assert_eq!(
        message,
        "satellite devbox terminal 7: its screen could not be delivered; open it again"
    );
    assert_eq!(session.take_delivery_detaches().len(), 1);
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
    assert_eq!(next.settle_undrawn().len(), 1, "one attach serves both");

    let (resume, _receivers) = lost();
    let mut restarted = session();
    restarted.set_incarnation(Some([2; 16]));
    assert!(resume.satellite_restarted(restarted.incarnation()));
    restarted.resume(resume);
    assert!(restarted.subscribers.is_empty());
    assert!(restarted.settle_undrawn().is_empty());
}

/// Whatever a satellite answers the hub's asks with, its viewers see only the
/// hub's error, at the end of the asks, and its other panes see nothing.
#[test]
fn a_refusal_of_the_hubs_ask_never_reaches_a_viewer() {
    let hostile = [
        (
            ErrorCode::VersionIncompatible,
            "unsupported protocol version",
        ),
        (ErrorCode::PermissionDenied, "denied"),
        (
            ErrorCode::SatelliteUnreachable,
            "satellite otherhost is unreachable: x",
        ),
        (ErrorCode::FrameTooLarge, "\u{1b}]0;title\u{7}"),
        (ErrorCode::Unknown(60_000), "?"),
    ];
    for (code, text) in hostile {
        let mut session = session();
        let (tx, mut rx) = mpsc::channel(8);
        let (other_tx, mut other_rx) = mpsc::channel(8);
        attach(&mut session, 7, ClientId(1), tx);
        attach(&mut session, 8, ClientId(2), other_tx);
        let eight = [Upstream(1).begin(0), Upstream(1).ready()];
        for frame in eight {
            session.feed(&on(8, frame));
        }
        let _ = frames(&mut other_rx);
        answer(&mut session, &CommandResult::Ok);
        for _ in 0..MAX_BOOTSTRAP_ASKS {
            assert_eq!(at_deadline(&mut session), 1, "refusals may be momentary");
            answer(&mut session, &CommandResult::error(code, text));
            assert_eq!(frames(&mut rx), Vec::new());
        }
        assert_eq!(at_deadline(&mut session), 0);
        assert_failed(&mut session, &mut rx);
        assert_eq!(frames(&mut other_rx), Vec::new());
        assert_eq!(session.subscribers[&8].len(), 1);
    }
}

/// A resize under the capture refuses one attach; the next one draws.
#[test]
fn a_momentary_refusal_is_asked_again_soon() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), tx);
    answer(&mut session, &CommandResult::Ok);
    assert_eq!(at_deadline(&mut session), 1);
    assert!(session.settle_undrawn().is_empty(), "one at a time");
    answer(&mut session, &refusal(ErrorCode::CodecUnavailable));
    assert!(wait_left(&mut session) < Duration::from_secs(2));
    assert_eq!(at_deadline(&mut session), 1);
    for frame in [Upstream(2).begin(0), Upstream(2).ready()] {
        session.feed(&frame);
    }
    answer(&mut session, &CommandResult::Ok);
    assert!(session.settle_undrawn().is_empty());
    assert!(
        viewer(&mut session)
            .content
            .as_ref()
            .expect("content")
            .deadline
            .is_none()
    );
    assert_eq!(frames(&mut rx).len(), 2);
}

/// A terminal that closed while the link was down closes for its viewers.
#[test]
fn an_ask_for_a_closed_terminal_closes_it() {
    let mut session = session();
    let (tx, mut rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), tx);
    answer(&mut session, &CommandResult::Ok);
    assert_eq!(at_deadline(&mut session), 1);
    answer(&mut session, &refusal(ErrorCode::TerminalNotFound));
    let told = frames(&mut rx);
    assert!(
        matches!(told.as_slice(), [FrameKind::ResourceClosed { .. }]),
        "{told:?}"
    );
    assert!(!session.subscribers.contains_key(&7));
}

/// Every way a content subscriber can be left undrawn has a deadline, at which
/// the hub asks: no case needs its own event.
#[test]
fn every_undrawn_viewer_has_a_deadline() {
    type Case = (&'static str, fn(&mut RelaySession));
    let cases: [Case; 7] = [
        ("attach unanswered", |_| ()),
        ("answered with nothing", |s| answer(s, &CommandResult::Ok)),
        ("silent after BEGIN", |s| {
            s.feed(&Upstream(1).begin(0));
            answer(s, &CommandResult::Ok);
        }),
        ("tombstoned without a replacement", |s| {
            for frame in [
                Upstream(1).begin(0),
                Upstream(1).ready(),
                Upstream(1).tombstone(),
            ] {
                s.feed(&frame);
            }
            answer(s, &CommandResult::Ok);
        }),
        ("another's attach refused", |s| {
            let (tx, _) = mpsc::channel(8);
            attach(s, 7, ClientId(2), tx);
            let mut ids: Vec<u32> = s.pending.keys().copied().collect();
            ids.sort_unstable();
            s.feed(&FrameKind::CommandResult {
                request_id: ids[0],
                result: CommandResult::Ok,
            });
            s.feed(&FrameKind::CommandResult {
                request_id: ids[1],
                result: refusal(ErrorCode::PermissionDenied),
            });
        }),
        ("another's attach abandoned", |s| {
            answer(s, &CommandResult::Ok);
            let (tx, _) = mpsc::channel(8);
            attach(s, 7, ClientId(2), tx);
            s.prune_abandoned();
        }),
        ("tombstone retained behind output", |s| {
            for frame in [Upstream(1).begin(0), Upstream(1).ready()] {
                s.feed(&frame);
            }
            answer(s, &CommandResult::Ok);
            let sub = viewer(s);
            sub.gate = SnapshotGate::Retained {
                frames: VecDeque::new(),
                retained_bytes: 0,
                open_after: true,
            };
            s.feed(&Upstream(1).tombstone());
            s.flush_pending_snapshots();
            assert!(matches!(viewer(s).gate, SnapshotGate::Open));
        }),
    ];
    for (case, leave_undrawn) in cases {
        let mut session = session();
        let (tx, _rx) = mpsc::channel(32);
        attach(&mut session, 7, ClientId(1), tx);
        leave_undrawn(&mut session);
        assert!(session.settle_undrawn().is_empty(), "{case}: early");
        assert!(wait_left(&mut session) <= RELAY_COMMAND_TIMEOUT, "{case}");
        if let Some(flow) = session.bootstrap_flows.get_mut(&7) {
            flow.progress_at = Instant::now()
                .checked_sub(RELAY_COMMAND_TIMEOUT)
                .expect("boot");
        }
        assert_eq!(at_deadline(&mut session), 1, "{case}");
    }
}

/// A bootstrap that keeps arriving is left alone however long it takes; so is
/// an attach not yet answered, whose own wait is the longest there is.
#[test]
fn a_slow_bootstrap_is_not_restarted() {
    let mut session = session();
    let (tx, _rx) = mpsc::channel(32);
    attach(&mut session, 7, ClientId(1), tx);
    session.feed(&Upstream(1).begin(0));
    for chunk_seq in 0..4 {
        session.feed(&Upstream(1).chunk(chunk_seq));
        viewer(&mut session)
            .content
            .as_mut()
            .expect("content")
            .deadline = Some(Instant::now());
        assert!(
            session.settle_undrawn().is_empty(),
            "chunk {chunk_seq} just came"
        );
    }
    assert!(wait_left(&mut session) > RELAY_COMMAND_TIMEOUT / 2);
}

/// Only an attach's answer says what an attach delivered.
#[test]
fn an_event_subscription_answer_hurries_nothing() {
    let mut session = session();
    let (tx, _rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), tx.clone());
    session.pending.clear();
    assert!(session.settle_undrawn().is_empty());
    let (reply, _answer) = oneshot::channel();
    session.handle_request(RelayRequest::Command {
        command: Command::SubscribeResourceEvents {
            terminal_id: ResourceId::local(7),
            event_types: Vec::new(),
        },
        reply,
        subscribe: Some(proxy(7, ClientId(1), tx)),
    });
    answer(&mut session, &CommandResult::Ok);
    assert!(wait_left(&mut session) > RELAY_COMMAND_TIMEOUT / 2);
}

/// A viewer that was failed neither streams nor types into the pane behind
/// its frozen screen.
#[test]
fn a_failed_viewer_loses_its_typing_permission() {
    let state = crate::state::SharedState::new();
    let mut session = session();
    session.set_journal(Some(state.clone()));
    let host = host();
    for client in [ClientId(1), ClientId(2)] {
        state.with_mut(|s| {
            s.federation_mut()
                .register_satellite_proxy_attach(client, host.clone(), 7);
        });
    }
    let (tx, mut rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), tx);
    answer(&mut session, &CommandResult::Ok);
    for _ in 0..MAX_BOOTSTRAP_ASKS {
        assert_eq!(at_deadline(&mut session), 1);
        answer(&mut session, &CommandResult::Ok);
    }
    assert_eq!(at_deadline(&mut session), 0);
    assert_failed(&mut session, &mut rx);
    let may_type =
        |client| state.with(|s| s.federation().has_satellite_proxy_attach(client, &host, 7));
    assert!(!may_type(ClientId(1)));
    assert!(may_type(ClientId(2)), "only the failed viewer");
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
        assert!(session.settle_undrawn().is_empty(), "not at once");
        waits.push(wait_left(&mut session));
        assert_eq!(at_deadline(&mut session), 1);

        // The fresh one is delivered whole.
        let fresh = Upstream(generation * 2 + 1);
        for frame in [fresh.begin(0), fresh.ready()] {
            session.feed(&frame);
            assert_eq!(received(&mut rx).len(), 1);
        }
        answer(&mut session, &CommandResult::Ok);
        assert!(session.settle_undrawn().is_empty());
        assert!(matches!(viewer(&mut session).gate, SnapshotGate::Open));
    }
    assert!(
        waits.windows(2).all(|pair| pair[1] > pair[0] * 2),
        "{waits:?}"
    );
    assert!(waits[0] < Duration::from_millis(300), "the first is soon");

    overflow(&mut session, &mut rx, 99);
    assert_eq!(at_deadline(&mut session), 0);
    assert_failed(&mut session, &mut rx);
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
