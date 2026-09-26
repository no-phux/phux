//! ADR-0033's `ttl_ms`, through the production frame loop (this lane,
//! "lease-ttl-and-holder"): a held lease expires after its TTL, a
//! release/seize/disconnect before expiry cancels the timer, `GET_STATE`
//! reports the holder, and a `SEIZE` rearms the TTL at the new holder's
//! value.
//!
//! `owner` attaches the seed pane; `driver` acquires/releases input leases
//! over it; `events` is a *journal-aware* subscription (`after_seq: Some`)
//! so `EXPIRED` crosses it as itself rather than rendered `RELEASED`
//! (L1 §7.1) — that per-subscriber rendering is L6's, exercised here only
//! incidentally by choosing a journal-aware watcher for every assertion
//! that needs to tell `Expired` apart from an explicit `Released`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::Path;
use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, FrameKind, InputMode,
    StateScope, TYPE_ATTACHED, TYPE_HELLO_OK,
};
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, await_command_result,
    join_after_shutdown, recv_typed, run_local, send_frame, spawn_server_with, wait_for_raw_socket,
};

const SESSION: &str = "demo";

/// One observed `EVENT` frame.
#[derive(Debug, Clone)]
struct Seen {
    event: AgentEvent,
}

fn as_seen(frame: FrameKind) -> Option<Seen> {
    match frame {
        FrameKind::Event { event, .. } => Some(Seen { event }),
        _ => None,
    }
}

/// `HELLO` as `name`; returns `HELLO_OK.server_id`.
async fn hello(stream: &mut UnixStream, name: &str) {
    send_frame(
        stream,
        &FrameKind::Hello {
            client_name: name.to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK);
    assert!(matches!(frame, FrameKind::HelloOk { .. }));
}

async fn connect(socket: &Path, name: &str) -> UnixStream {
    let mut stream = wait_for_raw_socket(socket, SOCKET_CONNECT_DEADLINE).await;
    hello(&mut stream, name).await;
    stream
}

/// Attach to `SESSION`; returns its focused pane.
async fn attach_pane(stream: &mut UnixStream) -> ResourceId {
    send_frame(stream, &attach_by_name(SESSION)).await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED);
    let FrameKind::Attached { snapshot, .. } = frame else {
        panic!("expected ATTACHED, got {frame:?}");
    };
    snapshot.focused_resource
}

async fn command(stream: &mut UnixStream, request_id: u32, command: Command) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

/// `SUBSCRIBE_EVENTS`, journal-aware (`after_seq: Some(u64::MAX)`, "live
/// only, but decode `EXPIRED` as itself" per L1 §7.1), then the `GET_STATE`
/// barrier so the subscription is installed before this returns.
async fn subscribe_journal_aware(stream: &mut UnixStream, request_id: u32) {
    send_frame(
        stream,
        &FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: Some(u64::MAX),
        },
    )
    .await;
    let result = command(
        stream,
        request_id,
        Command::GetState {
            scope: StateScope::Server,
        },
    )
    .await;
    assert!(
        !matches!(result, CommandResult::Error { .. }),
        "barrier: {result:?}"
    );
}

/// The next `EVENT` `matches` accepts, within [`WIRE_RECV_TIMEOUT`].
async fn next_event(stream: &mut UnixStream, matches: impl Fn(&Seen) -> bool) -> Seen {
    loop {
        let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the event arrives within the deadline");
        if let Some(seen) = as_seen(frame)
            && matches(&seen)
        {
            return seen;
        }
    }
}

/// Nothing matching `matches` arrives within `quiet`. Bounded, not a hang
/// guard: this one *is* a timing assertion, same discipline as
/// `retain_on_exit.rs`'s `QUIET`.
async fn assert_quiet(stream: &mut UnixStream, quiet: Duration, matches: impl Fn(&Seen) -> bool) {
    let outcome = timeout(quiet, next_event(stream, matches)).await;
    assert!(outcome.is_err(), "unexpected event within the quiet window");
}

fn is_terminal_control(action: ControlAction) -> impl Fn(&Seen) -> bool {
    move |seen: &Seen| {
        matches!(
            seen.event,
            AgentEvent::TerminalControl { action: a, .. } if a == action
        )
    }
}

async fn get_state_pane(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &ResourceId,
) -> phux_protocol::wire::info::ResourceInfo {
    let result = command(
        stream,
        request_id,
        Command::GetState {
            scope: StateScope::Server,
        },
    )
    .await;
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("expected GET_STATE OkWith(State), got {result:?}");
    };
    snapshot
        .resources
        .into_iter()
        .find(|r| &r.id == pane)
        .expect("the pane is in the snapshot")
}

fn start(
    configure: impl FnOnce(&mut phux_server::ServerConfig),
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let socket = tmp.path().join("phux.sock");
    let (shutdown, server) = spawn_server_with(socket.clone(), Some(SESSION), configure);
    (tmp, socket, shutdown, server)
}

#[test]
fn a_lease_with_a_ttl_expires_and_broadcasts_expired() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start(|_| {});
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe_journal_aware(&mut events, 1).await;
        let mut driver = connect(&socket, "driver").await;

        let result = command(
            &mut driver,
            2,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 150,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Acquired)).await;

        let expired = next_event(&mut events, is_terminal_control(ControlAction::Expired)).await;
        let AgentEvent::TerminalControl {
            input_holder,
            actor,
            ..
        } = expired.event
        else {
            unreachable!()
        };
        assert_eq!(input_holder, None, "EXPIRED returns the pane to Open");
        assert_eq!(actor, None, "nobody acted; the server's own timer did");

        let info = get_state_pane(&mut driver, 3, &pane).await;
        assert_eq!(info.input_holder, None, "GET_STATE reflects the expiry");

        drop((owner, events, driver));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_release_before_expiry_cancels_the_timer() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start(|_| {});
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe_journal_aware(&mut events, 1).await;
        let mut driver = connect(&socket, "driver").await;

        let result = command(
            &mut driver,
            2,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 200,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Acquired)).await;

        let result = command(
            &mut driver,
            3,
            Command::ReleaseInput {
                terminal_id: pane.clone(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Released)).await;

        // The armed 200ms timer must never fire: wait past it and see
        // nothing but silence on the terminal_control channel.
        assert_quiet(&mut events, Duration::from_millis(500), |seen| {
            matches!(seen.event, AgentEvent::TerminalControl { .. })
        })
        .await;

        drop((owner, events, driver));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn get_state_reports_the_input_holder() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start(|_| {});
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut driver = connect(&socket, "driver").await;

        let before = get_state_pane(&mut driver, 1, &pane).await;
        assert_eq!(before.input_holder, None, "no lease yet: Open");

        let result = command(
            &mut driver,
            2,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 0,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);

        let held = get_state_pane(&mut driver, 3, &pane).await;
        assert!(
            held.input_holder.is_some(),
            "GET_STATE names a holder once one exists"
        );

        let result = command(
            &mut driver,
            4,
            Command::ReleaseInput {
                terminal_id: pane.clone(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);

        let after = get_state_pane(&mut driver, 5, &pane).await;
        assert_eq!(after.input_holder, None, "released: back to Open");

        drop((owner, driver));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn seize_resets_the_ttl_to_the_new_holders_value() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start(|_| {});
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe_journal_aware(&mut events, 1).await;
        let mut first = connect(&socket, "first").await;
        let mut second = connect(&socket, "second").await;

        // `first` takes a long lease that would not expire during this
        // test if it were the one still governing the pane.
        let result = command(
            &mut first,
            2,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 60_000,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Acquired)).await;

        // `second` seizes with a short TTL. If SEIZE rearmed the timer at
        // the new holder's value, this pane expires shortly; if the old
        // 60s timer were still the one in force, it would not.
        let result = command(
            &mut second,
            3,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Seize,
                ttl_ms: 150,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Seized)).await;

        let expired = next_event(&mut events, is_terminal_control(ControlAction::Expired)).await;
        let AgentEvent::TerminalControl { input_holder, .. } = expired.event else {
            unreachable!()
        };
        assert_eq!(input_holder, None);

        drop((owner, events, first, second));
        join_after_shutdown(shutdown, server).await;
    });
}

/// Review round 2's high finding, reproduced: the first cut derived a
/// pane's next expiry generation from its own (always-cleared-on-release)
/// map entry, so a released-then-reacquired pane restarted at generation
/// `1` every time. A dead timer from the *first* (short) lease could then
/// share that generation with the *second* (long) lease's grant, and would
/// wake at the first lease's deadline believing itself still current —
/// expiring a lease that should have run for another minute.
#[test]
fn a_release_then_reacquire_does_not_let_the_dead_timer_expire_the_new_lease() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start(|_| {});
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe_journal_aware(&mut events, 1).await;
        let mut driver = connect(&socket, "driver").await;

        // Arm a short-lived lease...
        let result = command(
            &mut driver,
            2,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 300,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Acquired)).await;

        // ...release it before it fires...
        let result = command(
            &mut driver,
            3,
            Command::ReleaseInput {
                terminal_id: pane.clone(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Released)).await;

        // ...and re-acquire with a much longer TTL. The dead 300ms timer
        // must not be handed this grant's generation.
        let result = command(
            &mut driver,
            4,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 60_000,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Acquired)).await;

        // Past the dead timer's original 300ms deadline, the new lease
        // must still be untouched.
        assert_quiet(&mut events, Duration::from_millis(800), |seen| {
            matches!(seen.event, AgentEvent::TerminalControl { .. })
        })
        .await;

        drop((owner, events, driver));
        join_after_shutdown(shutdown, server).await;
    });
}

/// The same generation-reuse bug, reached through a `ttl_ms = 0`
/// re-acquire instead of an explicit release: any path that clears the
/// tracked generation (not just `RELEASE_INPUT`) must not let a later
/// grant reuse it.
#[test]
fn a_ttl_zero_reacquire_does_not_let_the_dead_timer_expire_the_new_lease() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start(|_| {});
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe_journal_aware(&mut events, 1).await;
        let mut driver = connect(&socket, "driver").await;

        // Arm a short-lived lease...
        let result = command(
            &mut driver,
            2,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 300,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Acquired)).await;

        // ...the same holder re-acquires with no TTL, clearing the tracked
        // generation without arming a replacement timer...
        let result = command(
            &mut driver,
            3,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 0,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Acquired)).await;

        // ...then finally arms a long TTL. The still-sleeping 300ms timer
        // must not be handed this grant's generation either.
        let result = command(
            &mut driver,
            4,
            Command::AcquireInput {
                terminal_id: pane.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 60_000,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        let _ = next_event(&mut events, is_terminal_control(ControlAction::Acquired)).await;

        assert_quiet(&mut events, Duration::from_millis(800), |seen| {
            matches!(seen.event, AgentEvent::TerminalControl { .. })
        })
        .await;

        drop((owner, events, driver));
        join_after_shutdown(shutdown, server).await;
    });
}
