use std::time::Duration;

use futures_util::FutureExt as _;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;

use super::lane::{InputLane, spawn_input_lane_with_completion_timeout};
use super::*;
use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use phux_protocol::{MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::state::{ClientId, SharedState, TerminalInput};
use crate::terminal_actor::EncodedInputRequest;
use crate::terminal_actor::{
    DEFAULT_INPUT_MAILBOX, INPUT_CREDITS, InputCreditPool, TerminalActor, WriteCompletion,
};

/// Bound for "this work was handed off" waits. Never the assertion
/// itself; generous so a loaded machine does not fail a correct lane.
const LANE_DELIVERY_DEADLINE: Duration = Duration::from_secs(30);

/// "Nothing else arrives" window: expiry is the pass.
const NOTHING_FURTHER_WINDOW: Duration = Duration::from_millis(100);

fn paste_event(bytes: &[u8]) -> PasteEvent {
    PasteEvent {
        trust: PasteTrust::Trusted,
        data: bytes.to_vec(),
    }
}

/// A trusted paste; with DEC 2004 off it encodes to exactly `bytes`.
fn paste(bytes: &[u8]) -> TerminalInput {
    TerminalInput::Paste(paste_event(bytes))
}

fn ev(bytes: &[u8]) -> InputEvent {
    InputEvent::Paste(paste_event(bytes))
}

fn operation_id(byte: u8) -> InputOperationId {
    InputOperationId::new([byte; 16]).expect("non-zero operation id")
}

fn code(result: &CommandResult) -> Option<ErrorCode> {
    match result {
        CommandResult::Error { code, .. } => Some(*code),
        _ => None,
    }
}

async fn next_write(rx: &mut mpsc::Receiver<EncodedInputRequest>) -> EncodedInputRequest {
    tokio::time::timeout(LANE_DELIVERY_DEADLINE, rx.recv())
        .await
        .expect("input reaches the PTY writer")
        .expect("writer channel open")
}

fn complete(request: EncodedInputRequest, outcome: WriteCompletion) {
    request
        .completion
        .expect("acknowledged completion")
        .complete(outcome);
}

async fn assert_no_write(rx: &mut mpsc::Receiver<EncodedInputRequest>) {
    assert!(
        tokio::time::timeout(NOTHING_FURTHER_WINDOW, rx.recv())
            .await
            .is_err(),
        "nothing further may reach the PTY writer"
    );
}

/// A live pane actor with test PTY channels, registered in its own
/// session. Must run inside a `LocalSet`.
struct Pane {
    wire: phux_protocol::ids::ResourceId,
    core: phux_core::ids::ResourceId,
    credits: InputCreditPool,
    writer_rx: mpsc::Receiver<EncodedInputRequest>,
    token: CancellationToken,
}

fn spawn_pane(state: &SharedState, session: &str, seed: &[u8]) -> Pane {
    let bundle = TerminalActor::new_with_seed(80, 24, seed).expect("actor");
    let handle = bundle.handle.clone();
    let credits = handle.terminal().expect("facet").input_credits.clone();
    let token = bundle.token.clone();
    let mut actor = bundle.actor;
    let (_pty_evt_tx, writer_rx) = actor.install_test_pty_channels();
    let (wire, core) = state.with_mut(|s| {
        let (_sid, core) = s.seed_session(session);
        (
            s.register_resource_handle(core, handle, token.clone()),
            core,
        )
    });
    tokio::task::spawn_local(actor.run());
    Pane {
        wire,
        core,
        credits,
        writer_rx,
        token,
    }
}

/// A pane plus two attached clients and a lane.
struct Fixture {
    state: SharedState,
    pane: Pane,
    client_a: ClientId,
    client_b: ClientId,
    lane: InputLane,
}

fn fixture_with_seed(seed: &[u8]) -> Fixture {
    let state = SharedState::new();
    let pane = spawn_pane(&state, "s", seed);
    let (client_a, client_b) = state.with_mut(|s| {
        let a = s.new_client_id();
        let b = s.new_client_id();
        s.attach_default_caps(a, "s", mpsc::channel(16).0)
            .expect("attach a");
        s.attach_default_caps(b, "s", mpsc::channel(16).0)
            .expect("attach b");
        (a, b)
    });
    let lane = spawn_input_lane(state.clone()).expect("spawn lane");
    Fixture {
        state,
        pane,
        client_a,
        client_b,
        lane,
    }
}

fn fixture() -> Fixture {
    fixture_with_seed(b"")
}

impl Fixture {
    fn handle(&self) -> InputLaneHandle {
        self.lane.handle()
    }

    fn attached(&self, client: ClientId, bytes: &[u8]) {
        self.handle()
            .route(RoutedInput::attached(
                client,
                self.pane.wire.clone(),
                paste(bytes),
                "INPUT_PASTE",
            ))
            .now_or_never()
            .expect("the lane queue has room");
    }

    /// Client A's `APPLY_INPUT` of one paste per payload to this pane.
    fn apply(&self, id: u8, payloads: &[&[u8]]) -> InputReceipt {
        let events = payloads.iter().map(|bytes| ev(bytes)).collect();
        self.handle().begin_apply(
            &self.state,
            self.client_a,
            operation_id(id),
            self.pane.wire.clone(),
            events,
        )
    }

    fn route(&self, client: ClientId, bytes: &[u8]) -> InputReceipt {
        self.handle()
            .begin_route(&self.state, client, self.pane.wire.clone(), ev(bytes), None)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.pane.token.cancel();
    }
}

mod acknowledged;
mod apply;
mod refusal;
mod routing;
mod saturation;
