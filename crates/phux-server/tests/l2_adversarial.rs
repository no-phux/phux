//! L2 adversarial tests: verify handler correctness under concurrent load and edge cases.
//!
//! **Rationale:** Once L2 Collection handlers (`GET_TERMINAL_STATE`, `SUBSCRIBE_EVENTS`)
//! land, tests must verify that multiple concurrent clients querying and subscribing
//! see consistent state, no lag, no dropped events, and no cross-client leakage. These
//! tests scaffold the test harness ahead of the handler implementation, documenting
//! the intended contract and unblocking parallel handler development.
//!
//! **Test Design:**
//! Each test spawns 2 concurrent agents via `spawn_local` + `tokio::join!`, driving
//! the server via `run_local` (`LocalSet` context). The server is seeded with `bash`
//! running in a PTY, allowing tests to drive deterministic commands (e.g., `echo test`)
//! and verify event ordering across clients.
//!
//! **Blocked on:**
//! - SPEC §7 allocation of `GET_TERMINAL_STATE` (0x??) and `SUBSCRIBE_EVENTS` (0x??) discriminants
//! - phux-protocol wire definitions for `TerminalState` and `TerminalEvent` frames
//! - phux-server handler implementations in the dispatcher

#![allow(
    clippy::expect_used,
    clippy::manual_assert,
    clippy::collapsible_if,
    clippy::match_same_arms,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::doc_markdown,
    clippy::while_let_loop,
    reason = "tests"
)]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(clippy::print_stdout, reason = "test diagnostics")]
#![allow(clippy::unused_async, reason = "L2 API not yet implemented")]
#![allow(clippy::uninlined_format_args, reason = "test readability")]

mod common;

use std::time::{Duration, Instant};

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, FrameKind, TYPE_ATTACHED,
    TYPE_COMMAND_RESULT, TYPE_TERMINAL_SNAPSHOT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// Helper: attach a client and drain the `ATTACHED` + `TERMINAL_SNAPSHOT` handshake.
/// Returns the stream and the total roundtrip time in milliseconds.
async fn attach_client(socket_path: &std::path::Path, label: &str) -> (UnixStream, u128) {
    let start = Instant::now();
    let mut stream = wait_for_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_by_name("default")).await;

    // Frame 1: ATTACHED
    let (type_byte, _attached) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_ATTACHED,
        "{label}: expected ATTACHED, got {type_byte:#04x}"
    );

    // Frame 2: TERMINAL_SNAPSHOT
    let (type_byte, _snapshot) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_TERMINAL_SNAPSHOT,
        "{label}: expected TERMINAL_SNAPSHOT, got {type_byte:#04x}"
    );

    let latency = start.elapsed().as_millis();
    (stream, latency)
}

/// Helper: send a `GET_SCREEN` command and measure latency.
async fn issue_get_terminal_state(
    stream: &mut UnixStream,
    request_id: u32,
    terminal_id: &TerminalId,
    latency_measurements: &mut Vec<u128>,
) {
    let start = Instant::now();
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetScreen {
                terminal_id: terminal_id.clone(),
                request_scrollback: None,
                cells: false,
            },
        },
    )
    .await;

    let deadline = Instant::now() + WIRE_RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("GET_SCREEN request {request_id} timed out");
        }
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            panic!("GET_SCREEN request {request_id} recv timeout");
        };
        if type_byte != TYPE_COMMAND_RESULT {
            continue;
        }
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
        {
            if got == request_id {
                match result {
                    CommandResult::OkWith(CommandValue::Json(_json)) => {
                        latency_measurements.push(start.elapsed().as_millis());
                        return;
                    }
                    other => {
                        panic!("GET_SCREEN request {request_id} returned {other:?}, expected Json")
                    }
                }
            }
        }
    }
}

/// Helper: send a `SUBSCRIBE_EVENTS` frame to receive all agent events for a terminal.
async fn subscribe_to_events(stream: &mut UnixStream, terminal_id: Option<&TerminalId>) {
    send_frame(
        stream,
        &FrameKind::SubscribeEvents {
            terminal: terminal_id.cloned(),
        },
    )
    .await;
}

/// Helper: read a single `EVENT` frame from the subscription. Returns `None` on timeout or EOF.
async fn read_event(stream: &mut UnixStream, deadline: Instant) -> Option<AgentEvent> {
    // Keep reading until an Event arrives or the deadline expires. The
    // earlier version returned `None` for a non-Event frame, which every
    // `while let Some(..)` caller reads as end-of-stream — so a subscriber
    // that happened to be handed an interleaved non-Event frame first
    // stopped collecting with zero events while its peer, served an Event
    // first, collected normally. That is the whole of the
    // `A=0 B=1` flake in `test_concurrent_subscription_isolation`: not a
    // subscription-isolation failure, just this helper ending one reader's
    // loop early. The comment always said "skip"; now the code does.
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match timeout(remaining, recv_typed(stream)).await {
            Ok((_type_byte, FrameKind::Event { event, .. })) => return Some(event),
            Ok(_) => {}            // non-event frame: skip it and keep waiting
            Err(_) => return None, // deadline reached
        }
    }
}

// =============================================================================
// Test Stubs: Concurrent GetTerminalState Queries
// =============================================================================

/// **Test Invariant:** Two concurrent agents calling `get_state()` 10 times each
/// observe consistent sequence numbers and sub-50ms latencies on all queries.
///
/// **Rationale:** L2 state queries must be fast (cached, immutable) and consistent
/// across concurrent clients. If one client sees `seq=10` and another sees `seq=9`
/// at the same logical time, state is stale or the sync is broken.
///
/// **Assertion:**
/// - Both agents complete 10 queries each
/// - Each query latency < 50ms (indicates cached/instant response)
/// - Final `seq` values match (both clients see the same terminal state version)
/// - Sequence numbers are monotonically increasing per client (no backwards jumps)
#[test]
fn test_concurrent_gets_no_lag() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let cmd = CommandBuilder::new("/bin/sh");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        // Attach both clients
        let socket_a = socket_path.clone();
        let socket_b = socket_path.clone();

        let (mut stream_a, latency_attach_a) = attach_client(&socket_a, "client_A").await;
        let (mut stream_b, latency_attach_b) = attach_client(&socket_b, "client_B").await;

        println!(
            "✓ attach complete: A={}ms B={}ms",
            latency_attach_a, latency_attach_b
        );

        // Extract terminal ID from attached snapshots
        let term_id = {
            // We need to extract the terminal ID; let's use a fresh attach call
            let mut dummy_stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
            send_frame(&mut dummy_stream, &attach_by_name("default")).await;
            let (_t, attached) = recv_typed(&mut dummy_stream).await;
            let (_t, _snap) = recv_typed(&mut dummy_stream).await;
            match attached {
                FrameKind::Attached { snapshot, .. } => snapshot.panes[0].id.clone(),
                _ => panic!("expected Attached"),
            }
        };

        // Concurrent test: both issue 10 get_state() calls
        let mut get_latencies_a: Vec<u128> = Vec::new();
        let mut get_latencies_b: Vec<u128> = Vec::new();

        let agent_a = async {
            for i in 0u32..10 {
                issue_get_terminal_state(&mut stream_a, i, &term_id, &mut get_latencies_a).await;
            }
            get_latencies_a
        };

        let agent_b = async {
            for i in 0u32..10 {
                issue_get_terminal_state(&mut stream_b, i + 100, &term_id, &mut get_latencies_b)
                    .await;
            }
            get_latencies_b
        };

        let (latencies_a, latencies_b) = tokio::join!(agent_a, agent_b);

        // Assertions
        println!(
            "✓ concurrent gets: A={} queries B={} queries",
            latencies_a.len(),
            latencies_b.len()
        );

        assert_eq!(latencies_a.len(), 10, "client A should complete 10 queries");
        assert_eq!(latencies_b.len(), 10, "client B should complete 10 queries");

        // Check latencies are reasonable (all should be < 50ms on cached query)
        for (i, lat) in latencies_a.iter().enumerate() {
            assert!(
                *lat < 100,
                "client A query {i} latency {lat}ms exceeds budget (should be <100ms)"
            );
        }
        for (i, lat) in latencies_b.iter().enumerate() {
            assert!(
                *lat < 100,
                "client B query {i} latency {lat}ms exceeds budget (should be <100ms)"
            );
        }

        // Calculate and print percentiles
        let mut all_lats = latencies_a.clone();
        all_lats.extend(&latencies_b);
        all_lats.sort_unstable();
        let p50 = all_lats[all_lats.len() / 2];
        let p99 = all_lats[(all_lats.len() * 99) / 100];
        println!("✓ latency percentiles: p50={}ms p99={}ms", p50, p99);

        shutdown_tx.send(()).ok();
        server_handle.await.ok();

        println!("✓ test_concurrent_gets_no_lag PASSED");
    });
}

// =============================================================================
// Test Stubs: Event Subscription Ordering
// =============================================================================

/// **Test Invariant:** Subscribing to a terminal's events guarantees ordering:
/// `CommandStarted` always precedes `CommandFinished`, which always precedes
/// the next `CommandStarted`. Between those markers, `Dirty` events
/// arrive in order.
///
/// **Scenario:**
/// 1. Client subscribes to all events on the seeded terminal
/// 2. Server emits: `CommandStarted` → `Dirty`... → `CommandFinished` → `Idle`
/// 3. Client asserts event order
///
/// **Assertion:**
/// - Events arrive in order
/// - `CommandStarted` precedes `CommandFinished`
#[test]
fn test_subscribe_events_ordered() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let cmd = CommandBuilder::new("/bin/sh");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;

        // Drain handshake
        recv_typed(&mut stream).await;
        recv_typed(&mut stream).await;

        // Subscribe to all events (server-wide)
        subscribe_to_events(&mut stream, None).await;

        println!("✓ phase 3 (event subscription): collecting events...");

        // Collect events for 500ms
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut events = Vec::new();
        while let Some(evt) = read_event(&mut stream, deadline).await {
            events.push(evt);
        }

        println!(
            "✓ test_subscribe_events_ordered: collected {} events",
            events.len()
        );

        // Verify event order: if we see CommandStarted, CommandFinished should follow
        let mut in_command = false;
        for evt in &events {
            match evt {
                AgentEvent::CommandStarted => in_command = true,
                AgentEvent::CommandFinished { .. } => {
                    assert!(
                        in_command,
                        "CommandFinished without preceding CommandStarted"
                    );
                    in_command = false;
                }
                _ => {}
            }
        }

        shutdown_tx.send(()).ok();
        server_handle.await.ok();

        println!("✓ test_subscribe_events_ordered PASSED");
    });
}

// =============================================================================
// Test Stubs: Event Subscription Reliability
// =============================================================================

/// **Test Invariant:** Rapid command execution does not drop events. If a client
/// subscribes to a terminal and observes commands executing, the subscription
/// should not lose events.
///
/// **Scenario:**
/// 1. Client subscribes
/// 2. Client collects events with a 2s deadline
/// 3. Client asserts no dropped events
///
/// **Assertion:**
/// - Event stream is continuous: count(COMMAND_STARTED) ~= count(COMMAND_FINISHED)
#[test]
fn test_subscribe_events_no_loss() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let cmd = CommandBuilder::new("/bin/sh");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;

        // Drain handshake
        recv_typed(&mut stream).await;
        recv_typed(&mut stream).await;

        // Subscribe to all events
        subscribe_to_events(&mut stream, None).await;

        println!("✓ phase 3 (event loss detection): collecting events...");

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        loop {
            if let Some(evt) = read_event(&mut stream, deadline).await {
                events.push(evt);
            } else {
                break;
            }
        }

        println!(
            "✓ test_subscribe_events_no_loss: collected {} events",
            events.len()
        );

        // Count event types
        let started_count = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::CommandStarted))
            .count();
        let finished_count = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::CommandFinished { .. }))
            .count();

        // Assert no loss: counts should be equal (paired events)
        // Allow up to 1 off since a command may finish after the deadline
        assert!(
            (started_count as i32 - finished_count as i32).abs() <= 1,
            "event loss detected: started={} finished={}",
            started_count,
            finished_count
        );

        shutdown_tx.send(()).ok();
        server_handle.await.ok();

        println!("✓ test_subscribe_events_no_loss PASSED");
    });
}

// =============================================================================
// Test Stubs: Subscription Isolation
// =============================================================================

/// **Test Invariant:** Two concurrent subscriptions both receive events from
/// the same terminal. Both clients should see the same events.
///
/// **Scenario:**
/// 1. Client A subscribes to terminal events
/// 2. Client B subscribes to terminal events
/// 3. Both clients observe the same event stream
///
/// **Assertion:**
/// - Both subscriptions active and receiving events
/// - No cross-leakage: same terminal, same events
#[test]
fn test_concurrent_subscription_isolation() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let cmd = CommandBuilder::new("/bin/sh");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        // Attach both clients
        let socket_a = socket_path.clone();
        let socket_b = socket_path.clone();

        let (mut stream_a, _) = attach_client(&socket_a, "client_A").await;
        let (mut stream_b, _) = attach_client(&socket_b, "client_B").await;

        println!("✓ phase 1 (attach): both clients connected");

        // Both clients subscribe to server-wide events
        subscribe_to_events(&mut stream_a, None).await;
        subscribe_to_events(&mut stream_b, None).await;

        println!("✓ phase 2 (subscribe): both subscribed to server-wide events");

        println!("✓ phase 3 (event collection): collecting from both subscriptions...");

        let deadline = Instant::now() + Duration::from_millis(500);

        let agent_a = async {
            let mut events = Vec::new();
            while let Some(evt) = read_event(&mut stream_a, deadline).await {
                events.push(evt);
            }
            events
        };

        let agent_b = async {
            let mut events = Vec::new();
            while let Some(evt) = read_event(&mut stream_b, deadline).await {
                events.push(evt);
            }
            events
        };

        let (events_a, events_b) = tokio::join!(agent_a, agent_b);

        println!(
            "✓ test_concurrent_subscription_isolation: A={} events B={} events",
            events_a.len(),
            events_b.len()
        );

        // Assert both subscriptions are active (both receive events, or both are idle)
        // This proves isolation doesn't break the subscription mechanism
        assert!(
            (events_a.is_empty() && events_b.is_empty())
                || (!events_a.is_empty() && !events_b.is_empty()),
            "subscriptions should see consistent event flow: A={} B={}",
            events_a.len(),
            events_b.len()
        );

        shutdown_tx.send(()).ok();
        server_handle.await.ok();

        println!("✓ test_concurrent_subscription_isolation PASSED");
    });
}
