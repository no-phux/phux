//! Wire-level integration test for the `AgentEvent::Asked` stream
//! (SPEC §7.5, ADR-0022 'events', `phux-2sl6`).
//!
//! `Asked` is the control-plane carrier for a pending human-answerable
//! question: an in-pane agent that has blocked for input signals it, and a
//! subscriber on the `EVENT` stream receives the question (id, text,
//! suggested answers) without re-deriving it from the grid. This test pins
//! the server half of that contract from the wire's point of view:
//!
//! 1. Pre-seed a PTY-backed pane with a shell that, after a short delay (so
//!    the client wins the race to subscribe), sets its terminal title via
//!    OSC 2 to a `phux-ask` sentinel, then idles so the marker stays set.
//! 2. Attach a client and `SUBSCRIBE_EVENTS { terminal: None }`
//!    (server-wide).
//! 3. Assert an `Asked` event arrives carrying the parsed id, question, and
//!    suggestion list.
//!
//! v1 ask-trigger is OSC-driven. libghostty-vt does not surface OSC 9 / OSC
//! 777 desktop-notification escapes through its Rust API — title (OSC 0/2),
//! pwd (OSC 7), and bell are the only user-notification signals it exposes —
//! so an agent signals a pending ask by setting its title to a `phux-ask`
//! sentinel. Full agent-state detection (manifests / hooks / OSC-9
//! surfacing) is the follow-up phux-2sl6.4.
//!
//! The leading sleep is the same race-avoidance trick `agent_events.rs`
//! uses: the event stream is a best-effort accelerator, so a marker set
//! before the subscription lands is legitimately dropped; the seed defers
//! its observable output until the client has subscribed.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ErrorCode, FrameKind, TYPE_ATTACHED,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// A shell that defers its observable output so the test client wins the
/// race to `SUBSCRIBE_EVENTS` before the marker fires. After ~250ms it sets
/// the terminal title via OSC 2 to a `phux-ask` sentinel carrying an id, a
/// question, and a `?s=` suggestion list, then sleeps so the title (and thus
/// the pending ask) stays set while the test collects events.
///
/// The OSC 2 payload is `phux-ask[q1]:Deploy to prod??s=Yes|No|Hold` — the
/// `phux-ask` prefix, the `[q1]` id, the `Deploy to prod?` question, and the
/// `Yes`/`No`/`Hold` suggestions. (The doubled `??` is literal: one `?`
/// closes the question text, the second begins the `?s=` suffix.)
fn seed_with_ask_title() -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    // `printf` is POSIX. `\033]2;...\007` is OSC 2 set-title. The leading
    // sleep lets the client subscribe first; the trailing sleep keeps the
    // ask marker set across the collection window.
    cmd.arg(
        "sleep 0.25; \
         printf '\\033]2;phux-ask[q1]:Deploy to prod??s=Yes|No|Hold\\007'; \
         sleep 2",
    );
    cmd
}

/// Drain `EVENT` frames until an `Asked` event is seen, or `deadline`
/// elapses. Non-`EVENT` frames (`ATTACHED`, `TERMINAL_SNAPSHOT`,
/// `TERMINAL_OUTPUT`, etc.) are skipped — we assert on the event stream.
async fn collect_until_asked(stream: &mut UnixStream, deadline: Duration) -> Option<AgentEvent> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            return None;
        };
        if let FrameKind::Event { event, .. } = frame
            && matches!(event, AgentEvent::Asked { .. })
        {
            return Some(event);
        }
    }
}

async fn recv_command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    loop {
        let (_type_byte, frame) = recv_typed(stream).await;
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
}

async fn collect_result_and_asked(
    stream: &mut UnixStream,
    request_id: u32,
    deadline: Duration,
) -> (Option<CommandResult>, Option<AgentEvent>) {
    let end = tokio::time::Instant::now() + deadline;
    let mut result = None;
    let mut asked = None;
    while result.is_none() || asked.is_none() {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        match frame {
            FrameKind::CommandResult {
                request_id: got,
                result: got_result,
            } if got == request_id => result = Some(got_result),
            FrameKind::Event { event, .. } if matches!(event, AgentEvent::Asked { .. }) => {
                asked = Some(event);
            }
            _ => {}
        }
    }
    (result, asked)
}

/// A subscribed client receives an `Asked` agent event when the seed pane
/// sets a `phux-ask` title sentinel, carrying the parsed id, question, and
/// suggestions (SPEC §7.5, phux-2sl6).
#[test]
fn subscribed_client_receives_asked_event_from_ask_title() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let cmd = seed_with_ask_title();
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ---- ATTACH ---- (so the client has an `attached` mailbox the
        // event fanout can target).
        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED",
        );

        // ---- SUBSCRIBE_EVENTS (server-wide) ---- before the seed's
        // ~250ms deferred ask-title fires.
        send_frame(&mut stream, &FrameKind::SubscribeEvents { terminal: None }).await;

        // ---- collect until the Asked event arrives ----
        let asked = collect_until_asked(&mut stream, WIRE_RECV_TIMEOUT).await;

        let Some(AgentEvent::Asked {
            id,
            question,
            suggestions,
            elapsed_seconds,
        }) = asked
        else {
            panic!("expected an Asked event from the phux-ask title; got {asked:?}");
        };
        assert_eq!(id, "q1", "Asked.id must be the `[q1]` segment");
        assert_eq!(
            question, "Deploy to prod?",
            "Asked.question must be the title body before `?s=`",
        );
        assert_eq!(
            suggestions,
            vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
            "Asked.suggestions must be the `?s=`-delimited options, in order",
        );
        assert_eq!(
            elapsed_seconds, None,
            "v1 does not track elapsed-since-ask server-side",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

#[test]
fn report_asked_command_emits_asked_event() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 2");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected ATTACHED");
        };

        send_frame(&mut stream, &FrameKind::SubscribeEvents { terminal: None }).await;
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 7,
                command: Command::ReportAsked {
                    terminal_id: snapshot.focused_pane,
                    id: "hook-q1".to_owned(),
                    question: "Approve release?".to_owned(),
                    suggestions: vec!["Ship".to_owned(), "Hold".to_owned()],
                    elapsed_seconds: Some(12),
                },
            },
        )
        .await;
        let (result, asked) = collect_result_and_asked(&mut stream, 7, WIRE_RECV_TIMEOUT).await;
        assert_eq!(result, Some(CommandResult::Ok));
        let Some(AgentEvent::Asked {
            id,
            question,
            suggestions,
            elapsed_seconds,
        }) = asked
        else {
            panic!("expected an Asked event from REPORT_ASKED; got {asked:?}");
        };
        assert_eq!(id, "hook-q1");
        assert_eq!(question, "Approve release?");
        assert_eq!(suggestions, vec!["Ship".to_owned(), "Hold".to_owned()]);
        assert_eq!(elapsed_seconds, Some(12));

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

#[test]
fn report_asked_rejects_empty_question() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 2");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (_type_byte, attached) = recv_typed(&mut stream).await;
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected ATTACHED");
        };

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 8,
                command: Command::ReportAsked {
                    terminal_id: snapshot.focused_pane,
                    id: "bad".to_owned(),
                    question: "   ".to_owned(),
                    suggestions: Vec::new(),
                    elapsed_seconds: None,
                },
            },
        )
        .await;
        let CommandResult::Error { code, message } = recv_command_result(&mut stream, 8).await
        else {
            panic!("empty REPORT_ASKED question must be rejected");
        };
        assert_eq!(code, ErrorCode::InvalidCommand);
        assert!(message.contains("question"));

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
