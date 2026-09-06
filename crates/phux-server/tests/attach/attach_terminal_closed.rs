//! Wire-level integration test for `TERMINAL_CLOSED` delivery to an
//! `ATTACH_TERMINAL`-only consumer (`phux-w7z2.56`).
//!
//! [L1 §5.1](../../../docs/spec/L1.md) says `ATTACH_TERMINAL` "registers the
//! caller as an output subscriber" and that "a session-scoped `ATTACH` is not
//! required". [L1 §3.1](../../../docs/spec/L1.md) says the server "MUST emit
//! [`TERMINAL_CLOSED`] to every client subscribed to the Terminal". Together
//! those two sentences already required what this test asserts; the server
//! did not do it.
//!
//! The pane-EOF watcher resolved subscriber mailboxes through
//! `ClientTable::attached`, which only a session-scoped `ATTACH` ever
//! populates. A consumer that reached one pane through `ATTACH_TERMINAL` was
//! therefore on the pane's subscriber list and filtered straight back out of
//! the fanout: when the pane died it received nothing at all. Not an error,
//! not a close — the output simply stopped, which from the consumer's side is
//! indistinguishable from a pane that has gone quiet. That is the shape an
//! agent orchestrating panes is most exposed to (it watches one pane, it does
//! not attach to a session), and it is the same shape a federation hub's
//! proxy subscription takes, so a satellite pane's death never reached the
//! hub and left dead proxy state behind.
//!
//! Two consumers, one pane, one death:
//!
//! * `watcher` — connects, `HELLO`s, and sends **only** `ATTACH_TERMINAL`.
//!   This is the connection the bug silenced.
//! * `owner` — session-attached, and auto-subscribed to the pane it spawned.
//!   This one always worked; it is here to prove the fix delivers to it
//!   exactly once rather than twice (it is reachable both ways).
//!
//! No sleeps anywhere. The victim pane is killed on request rather than
//! timed out, and every ordering the test depends on is anchored on a
//! `CommandResult` — the per-connection frame loop processes in order, so a
//! reply proves everything sent ahead of it is already applied server-side.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::ids::{GroupId, TerminalId};
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, SpawnResult, StateScope, TYPE_ATTACHED,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed,
    run_local, send_frame, spawn_server_with_seed_cmd, wait_for_socket,
};

/// A shell that never exits on its own.
///
/// Both the seed pane and the victim pane run this. Nothing in this test is
/// timed: the victim dies from an explicit `KILL_TERMINAL` at the point the
/// test chooses, and the seed pane outliving everything keeps the session
/// populated so the last-pane server self-exit (phux-60s) never races the
/// assertions. A pane that exits on a timer is the phux-w266 flake class —
/// under load it dies before the collection window and the failure surfaces
/// as "early eof" rather than as anything about the contract.
fn immortal_shell() -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("while :; do sleep 3600; done");
    cmd
}

/// Drain until the `CommandResult` for `request_id` arrives, ignoring every
/// other frame. Only safe before the pane dies, when no `TERMINAL_CLOSED`
/// can be among the frames it discards; use [`count_extra_closed`] after.
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

/// Drain frames until a `TERMINAL_CLOSED` naming `victim` arrives, or
/// `deadline` elapses. Returns its `exit_status` on arrival.
///
/// The bound is the shared `WIRE_RECV_TIMEOUT`, and it is not load-bearing:
/// what is asserted is arrival, never latency. A server that never emits the
/// frame — the phux-w7z2.56 regression — still fails, at the ceiling.
async fn await_terminal_closed(
    stream: &mut UnixStream,
    victim: &TerminalId,
    deadline: Duration,
) -> Option<Option<i32>> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            return None;
        };
        if let FrameKind::TerminalClosed {
            terminal_id,
            exit_status,
        } = frame
            && terminal_id == *victim
        {
            return Some(exit_status);
        }
    }
}

/// Count further `TERMINAL_CLOSED` frames for `victim` up to the
/// `CommandResult` for `barrier_request_id`. Returns
/// `(extra_closed, barrier_seen)`.
///
/// This is what makes "exactly once" assertable without a sleep. A duplicate
/// would be written into this connection's mailbox inside the *same*
/// broadcast loop as the first frame — strictly before the server dequeues a
/// command sent afterwards — so a barrier reply with nothing behind it proves
/// there was no second frame. "Wait a bit and see" would only have proved the
/// wait was long enough on this machine.
async fn count_extra_closed(
    stream: &mut UnixStream,
    victim: &TerminalId,
    barrier_request_id: u32,
    deadline: Duration,
) -> (u32, bool) {
    let end = tokio::time::Instant::now() + deadline;
    let mut extra = 0u32;
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return (extra, false);
        }
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            return (extra, false);
        };
        match frame {
            FrameKind::TerminalClosed { terminal_id, .. } if terminal_id == *victim => {
                extra = extra.saturating_add(1);
            }
            FrameKind::CommandResult { request_id, .. } if request_id == barrier_request_id => {
                return (extra, true);
            }
            _ => {}
        }
    }
}

/// Ask for a server-scoped state snapshot. Read-only, needs no subscription
/// and no session-scoped `ATTACH`, and always answers — which is what makes
/// it usable as an ordering barrier on the `ATTACH_TERMINAL`-only
/// connection, where every terminal-scoped command would be gated on a
/// subscription to a pane that is by then dead.
const fn state_barrier(request_id: u32) -> FrameKind {
    FrameKind::Command {
        request_id,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    }
}

/// Spawn the pane this test kills, on the `owner` connection.
///
/// A second pane rather than the seed pane, so its death cannot empty the
/// server and trip the last-pane self-exit (phux-60s) while the test is
/// still reading. It runs the same never-exiting shell: nothing here is
/// allowed to die on a timer.
async fn spawn_victim_pane(owner: &mut UnixStream) -> TerminalId {
    send_frame(
        owner,
        &FrameKind::SpawnTerminal {
            request_id: 1,
            group: GroupId::new(1),
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "while :; do sleep 3600; done".to_owned(),
            ]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
        },
    )
    .await;
    loop {
        let (_type_byte, frame) = recv_typed(owner).await;
        if let FrameKind::TerminalSpawned { request_id, result } = frame
            && request_id == 1
        {
            match result {
                SpawnResult::Ok(id) => return id,
                other => panic!("SPAWN_TERMINAL failed: {other:?}"),
            }
        }
    }
}

/// Subscribe `watcher` to `victim` with `ATTACH_TERMINAL` and nothing else,
/// returning once the server has answered.
///
/// No `ATTACH` is sent on this connection, ever. It therefore never acquires
/// a session-attach record — precisely the state the old fanout could not
/// address.
async fn attach_terminal_only(watcher: &mut UnixStream, victim: &TerminalId) {
    send_frame(
        watcher,
        &FrameKind::Command {
            request_id: 100,
            command: Command::AttachTerminal {
                terminal_id: victim.clone(),
            },
        },
    )
    .await;
    let result = timeout(WIRE_RECV_TIMEOUT, recv_command_result(watcher, 100))
        .await
        .expect("the server must answer ATTACH_TERMINAL");
    assert!(
        matches!(result, CommandResult::Ok),
        "ATTACH_TERMINAL must succeed, got {result:?}",
    );
}

/// A consumer that reached a Terminal through `ATTACH_TERMINAL` alone —
/// never a session-scoped `ATTACH` — receives `TERMINAL_CLOSED` when that
/// Terminal dies, and the session-attached consumer receives it exactly once
/// (L1 §3.1, phux-w7z2.56).
///
/// Before the fix the `watcher` assertion below timed out at
/// `WIRE_RECV_TIMEOUT` having seen zero `TERMINAL_CLOSED` frames: the pane's
/// output just stopped.
#[test]
fn attach_terminal_only_consumer_receives_terminal_closed() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", immortal_shell());

        // ---- owner: HELLO + ATTACH, then spawn the victim pane ----
        let mut owner = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut owner, &attach_by_name("demo")).await;
        let (type_byte, _attached) = recv_typed(&mut owner).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED",
        );
        let victim = spawn_victim_pane(&mut owner).await;

        // ---- watcher: HELLO, then ATTACH_TERMINAL and nothing else ----
        let mut watcher = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        attach_terminal_only(&mut watcher, &victim).await;

        // `attach_terminal_only` returned on the `CommandResult`, which is
        // the registration barrier: the server processes one connection's
        // frames in order, so that reply proves the subscription is
        // installed. Only now may the pane die — the third shape of the
        // phux-w266 flake class is "the event fired before the observer was
        // registered", and a barrier anchored on `send_frame` returning
        // would not have closed it (that only proves the bytes left this
        // end).
        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 2,
                command: Command::KillTerminal {
                    terminal_id: victim.clone(),
                },
            },
        )
        .await;

        // ---- the contract under test ----
        //
        // The kill is asynchronous with respect to this connection (the
        // actor is cancelled, its EOF watcher fires, and only then does the
        // fanout run), so the wait is on the frame itself rather than on a
        // barrier: no barrier on THIS connection can order against work
        // driven from another one.
        let watcher_status = await_terminal_closed(&mut watcher, &victim, WIRE_RECV_TIMEOUT)
            .await
            .expect(
                "an ATTACH_TERMINAL-only consumer must receive TERMINAL_CLOSED for the pane \
                 it subscribed to (L1 §3.1); never receiving it is the phux-w7z2.56 regression",
            );

        // Now that the fanout has provably run, a barrier is meaningful:
        // anything it emitted for this pane is already queued ahead of the
        // reply.
        send_frame(&mut watcher, &state_barrier(101)).await;
        let (extra, barrier_seen) =
            count_extra_closed(&mut watcher, &victim, 101, WIRE_RECV_TIMEOUT).await;
        assert!(
            barrier_seen,
            "the server must answer the watcher's GET_STATE barrier",
        );
        assert_eq!(
            extra, 0,
            "the ATTACH_TERMINAL-only consumer must receive TERMINAL_CLOSED exactly once",
        );

        // ---- and the session-attached consumer, exactly once ----
        let owner_status = await_terminal_closed(&mut owner, &victim, WIRE_RECV_TIMEOUT)
            .await
            .expect("the session-attached consumer must still receive TERMINAL_CLOSED");
        assert_eq!(
            owner_status, watcher_status,
            "both consumers must observe the same lifecycle fact",
        );
        send_frame(&mut owner, &state_barrier(102)).await;
        let (owner_extra, owner_barrier_seen) =
            count_extra_closed(&mut owner, &victim, 102, WIRE_RECV_TIMEOUT).await;
        assert!(
            owner_barrier_seen,
            "the server must answer the owner's GET_STATE barrier",
        );
        assert_eq!(
            owner_extra, 0,
            "the session-attached consumer must still receive exactly one \
             TERMINAL_CLOSED, not two",
        );

        drop(watcher);
        drop(owner);
        shutdown_tx.send(()).ok();
        timeout(SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
