//! Wire-level integration test for the `AgentEvent::Asked` stream
//! (SPEC §7.5, ADR-0022 'events', `phux-2sl6`).
//!
//! `Asked` is the control-plane carrier for a pending human-answerable
//! question: an in-pane agent that has blocked for input signals it, and a
//! subscriber on the `EVENT` stream receives the question (id, text,
//! suggested answers) without re-deriving it from the grid. This test pins
//! the server half of that contract from the wire's point of view:
//!
//! 1. Pre-seed a PTY-backed pane with a shell that waits on a release file
//!    (so the client provably wins the race to subscribe), then sets its
//!    terminal title via OSC 2 to a `phux-ask` sentinel and idles so the
//!    marker stays set.
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
//! The release file exists because the event stream is a best-effort
//! accelerator: a marker set before the subscription lands is legitimately
//! dropped, so the seed defers its observable output until the client has
//! subscribed. "Subscribed" has to mean *the server processed the subscribe*
//! — `SUBSCRIBE_EVENTS` is answered with no frame, so the test forces the
//! ordering with a following command whose reply it waits for.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ErrorCode, FrameKind, TYPE_ATTACHED, TYPE_HELLO_OK,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_raw_socket,
};
async fn negotiate(stream: &mut UnixStream) {
    send_frame(
        stream,
        &FrameKind::Hello {
            client_name: "phux-agent-asked-test".to_owned(),
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
fn seed_with_ask_title(release: &std::path::Path) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    // `printf` is POSIX. `\033]2;...\007` is OSC 2 set-title. The title must
    // not fire until the client has subscribed, or the event it is waiting
    // for happens before it is listening.
    //
    // This was `sleep 0.25`, which is the same shape as the flakes fixed in
    // phux-w266: it bets that a subscribe completes inside a fixed window,
    // and loses that bet under parallel test load. The barrier removes the
    // bet — but only if the file is written once the server has *processed*
    // the subscribe, not merely once the client has sent it. See the barrier
    // comment at the write site.
    //
    // The trailing sleep was `2`, which was a second, subtler race: it bounds
    // how long the ask marker stays set, and the collector waits up to
    // `WIRE_RECV_TIMEOUT` (15s). Under load the pane could exit first, which
    // reaps the session, self-exits the server, and fails the test with
    // "early eof" rather than with anything about asks. The pane only has to
    // outlive the collection window, so it now comfortably does.
    cmd.arg(format!(
        "until [ -f '{}' ]; do sleep 0.01; done; \
         printf '\\033]2;phux-ask[q1]:Deploy to prod??s=Yes|No|Hold\\007'; \
         sleep 60",
        release.display()
    ));
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

        let release = tmp.path().join("release");
        let cmd = seed_with_ask_title(&release);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        negotiate(&mut stream).await;

        // ---- ATTACH ---- (so the client has an `attached` mailbox the
        // event fanout can target).
        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED",
        );
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected ATTACHED to carry a snapshot");
        };

        // ---- SUBSCRIBE_EVENTS (server-wide) ----
        send_frame(&mut stream, &FrameKind::SubscribeEvents { terminal: None }).await;

        // Wait until the server has *processed* the subscribe before letting
        // the seed emit the title.
        //
        // The previous barrier released on `send_frame` returning, which only
        // proves the bytes left this end. `SUBSCRIBE_EVENTS` carries no
        // request_id and is answered with no frame, so there is nothing to
        // wait for directly -- and under parallel load the title could still
        // fire before `handle_subscribe_events` ran, dropping the event the
        // test exists to observe. That is the residual half of the phux-w266
        // race class: not "the pane died too early" but "the event fired
        // before the observer was registered".
        //
        // A command that *does* reply, sent after the subscribe on the same
        // connection, is the barrier: the per-connection frame loop handles
        // frames in order, so a `CommandResult` for this request proves the
        // subscribe ahead of it is already installed. `GetTerminalState` is
        // read-only, so waiting on it changes nothing else.
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 1,
                command: Command::GetTerminalState {
                    terminal_id: snapshot.focused_pane,
                    include_scrollback: false,
                    max_scrollback_lines: 0,
                },
            },
        )
        .await;
        let barrier = timeout(WIRE_RECV_TIMEOUT, recv_command_result(&mut stream, 1))
            .await
            .expect("the server must answer GET_TERMINAL_STATE before the collection window");
        assert!(
            !matches!(barrier, CommandResult::Error { .. }),
            "the subscribe barrier must succeed, got {barrier:?}",
        );

        // Only now let the seed emit the ask title.
        std::fs::write(&release, b"go").expect("release the ask title");

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

        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        negotiate(&mut stream).await;
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

        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        negotiate(&mut stream).await;
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
