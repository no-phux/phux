//! Relay session tests: shared helpers; each file covers one responsibility.

use phux_protocol::wire::frame::{AgentEvent, CommandValue, StateScope};

use super::*;

pub(super) fn host() -> SatelliteHost {
    SatelliteHost::new("devbox")
}

pub(super) fn decode(bytes: &[u8]) -> FrameKind {
    FrameKind::decode(bytes).expect("frame").0
}

pub(super) fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}

/// Register a `SUBSCRIBE_EVENTS`-shaped subscription at token 1.
pub(super) fn subscribe(
    session: &mut RelaySession,
    terminal: u32,
    client: ClientId,
    out_tx: mpsc::Sender<Outbound>,
) {
    subscribe_at(session, terminal, client, 1, out_tx);
}

/// [`subscribe`] with an explicit issue-order token.
pub(super) fn subscribe_at(
    session: &mut RelaySession,
    terminal: u32,
    client: ClientId,
    seq: u64,
    out_tx: mpsc::Sender<Outbound>,
) {
    let wire = session.handle_request(RelayRequest::Subscribe {
        subscription: ProxySubscription {
            resource_kind: None,
            terminal,
            client,
            out_tx,
            consumer_cancel: CancellationToken::new(),
            seq,
            awaits_snapshot: false,
            bootstrap_profile: None,
            bootstrap_limits: None,
        },
        forward: FrameKind::SubscribeEvents {
            terminal: Some(ResourceId::local(terminal)),
            after_seq: None,
        },
    });
    assert!(
        !wire.is_empty(),
        "atomic subscribe must produce the forward frame"
    );
}

/// Register an `ATTACH_RESOURCE` subscription (starts gated).
pub(super) fn attach(
    session: &mut RelaySession,
    terminal: u32,
    client: ClientId,
    out_tx: mpsc::Sender<Outbound>,
) {
    let selected_profile = session.bootstrap_profile;
    let selected_limits = session.bootstrap_limits;
    let (reply, _rx) = oneshot::channel();
    let wire = session.handle_request(RelayRequest::Command {
        command: Command::attach_terminal(ResourceId::local(terminal), None),
        reply,
        subscribe: Some(ProxySubscription {
            resource_kind: None,
            terminal,
            client,
            out_tx,
            consumer_cancel: CancellationToken::new(),
            seq: 1,
            awaits_snapshot: true,
            bootstrap_profile: Some(selected_profile),
            bootstrap_limits: Some(selected_limits),
        }),
    });
    assert!(!wire.is_empty(), "attach must produce the COMMAND frame");
}

mod bootstrap;
mod capabilities;
mod commands;
mod events;
mod kinds;
mod leases;
mod listings;
mod mirror;
mod requests;
mod retention;
mod routing;
mod snapshots;
mod stream;
mod withdrawal;

use bootstrap::*;
use capabilities::*;
use requests::*;
use snapshots::*;
