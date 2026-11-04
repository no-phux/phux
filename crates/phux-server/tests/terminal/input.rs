//! Wire input reaching real PTYs: `INPUT_KEY`/`ROUTE_INPUT` ordering and
//! legacy encoding, mouse wheel, routed paste bracketing and trust, and
//! `ROUTE_INPUT`'s no-resize contract.

use std::time::Duration;

use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::tracing_capture::TracingCapture;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, ServerHandles, WIRE_RECV_TIMEOUT, ascii_key, attach_by_name, command,
    join_after_shutdown, recv_until, run_local, send_frame, spawn_server_with_seed_cmd,
    try_recv_typed, try_recv_typed_before, wait_for_socket,
};

use super::common::{focused_resource, named_key, poll_screen, sh};

/// A server seeding session `work` with `cmd`, plus one HELLO'd client.
async fn seeded(tmp: &TempDir, cmd: CommandBuilder) -> (ServerHandles, UnixStream) {
    let socket = tmp.path().join("phux.sock");
    let server = spawn_server_with_seed_cmd(socket.clone(), "work", cmd);
    (
        server,
        wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await,
    )
}

/// Attach to `work` and drain through `BOOTSTRAP_READY`; returns the pane.
async fn attach(stream: &mut UnixStream) -> ResourceId {
    send_frame(stream, &attach_by_name("work")).await;
    let pane = recv_until(stream, |_, frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot.resources[0].id.clone()),
        _ => None,
    })
    .await;
    recv_until(stream, |_, f| {
        matches!(f, FrameKind::BootstrapReady { .. }).then_some(())
    })
    .await;
    pane
}

async fn route(stream: &mut UnixStream, request_id: u32, pane: &ResourceId, event: InputEvent) {
    let cmd = Command::RouteInput {
        terminal_id: pane.clone(),
        event,
    };
    assert_eq!(command(stream, request_id, cmd).await, CommandResult::Ok);
}

fn key(terminal_id: &ResourceId, c: char, k: PhysicalKey) -> FrameKind {
    FrameKind::InputKey {
        terminal_id: terminal_id.clone(),
        event: ascii_key(c, k),
    }
}

/// `INPUT_KEY`, `ROUTE_INPUT`, `INPUT_KEY` sent back to back share one FIFO
/// into the pane, and plain letters reach a default-mode pane as legacy
/// ASCII (phux-7vx: never kitty CSI-u unless the app opts in), so `cat`
/// echoes `abc` contiguously.
#[test]
fn mixed_input_key_and_route_input_preserve_wire_order() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let ((shutdown, server), mut stream) = seeded(&tmp, CommandBuilder::new("/bin/cat")).await;
        let pane = attach(&mut stream).await;

        send_frame(&mut stream, &key(&pane, 'a', PhysicalKey::A)).await;
        let route_b = Command::RouteInput {
            terminal_id: pane.clone(),
            event: InputEvent::Key(ascii_key('b', PhysicalKey::B)),
        };
        let request = FrameKind::Command {
            request_id: 7,
            command: route_b,
        };
        send_frame(&mut stream, &request).await;
        send_frame(&mut stream, &key(&pane, 'c', PhysicalKey::C)).await;
        let enter = FrameKind::InputKey {
            terminal_id: pane.clone(),
            event: named_key(PhysicalKey::Enter),
        };
        send_frame(&mut stream, &enter).await;

        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let (mut acc, mut acked) = (Vec::new(), false);
        while !(acked && acc.windows(3).any(|w| w == b"abc")) {
            let Ok(Some((_, frame))) = try_recv_typed_before(&mut stream, deadline).await else {
                panic!("no ordered `abc` echo (acked={acked}): {acc:?}");
            };
            match frame {
                FrameKind::ResourceOutput { bytes, .. } => acc.extend_from_slice(&bytes),
                FrameKind::CommandResult {
                    request_id: 7,
                    result,
                } => {
                    assert_eq!(result, CommandResult::Ok);
                    acked = true;
                }
                _ => {}
            }
        }
        assert!(
            !acc.windows(3).any(|w| w == b"[97"),
            "kitty CSI-u leaked: {acc:?}"
        );

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

/// A printable writes on the receive task only when this pane has nothing
/// queued or in flight from any source. Each burst is sent with no wait
/// between its frames. One `APPLY_INPUT` per pane is in flight at a time, so
/// the bursts themselves are sequential.
#[test]
fn later_keys_wait_behind_routed_and_acknowledged_input() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let ((shutdown, server), mut stream) = seeded(&tmp, CommandBuilder::new("/bin/cat")).await;
        let pane = attach(&mut stream).await;

        // `ROUTE_INPUT` then `INPUT_KEY`.
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 11,
                command: Command::RouteInput {
                    terminal_id: pane.clone(),
                    event: InputEvent::Key(ascii_key('r', PhysicalKey::R)),
                },
            },
        )
        .await;
        send_frame(&mut stream, &key(&pane, 'k', PhysicalKey::K)).await;
        expect_echo(&mut stream, b"rk", &[11]).await;

        // `APPLY_INPUT` then `INPUT_KEY`.
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 12,
                command: Command::ApplyInput {
                    operation_id: InputOperationId::new([0x21; 16]).expect("non-zero id"),
                    terminal_id: pane.clone(),
                    events: vec![InputEvent::Key(ascii_key('p', PhysicalKey::P))],
                },
            },
        )
        .await;
        send_frame(&mut stream, &key(&pane, 'q', PhysicalKey::Q)).await;
        expect_echo(&mut stream, b"pq", &[12]).await;

        // `INPUT_KEY`, `APPLY_INPUT`, `INPUT_KEY`.
        send_frame(&mut stream, &key(&pane, 'a', PhysicalKey::A)).await;
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 13,
                command: Command::ApplyInput {
                    operation_id: InputOperationId::new([0x22; 16]).expect("non-zero id"),
                    terminal_id: pane.clone(),
                    events: vec![InputEvent::Key(ascii_key('m', PhysicalKey::M))],
                },
            },
        )
        .await;
        send_frame(&mut stream, &key(&pane, 'z', PhysicalKey::Z)).await;
        expect_echo(&mut stream, b"amz", &[13]).await;

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

/// Read until `needle` has been echoed and every `acks` request has succeeded.
async fn expect_echo(stream: &mut UnixStream, needle: &[u8], acks: &[u32]) {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let (mut acc, mut seen) = (Vec::new(), 0usize);
    while seen < acks.len() || !acc.windows(needle.len()).any(|window| window == needle) {
        let Ok(Some((_, frame))) = try_recv_typed_before(stream, deadline).await else {
            panic!(
                "mixed sources reordered or dropped waiting for {:?} (acks {seen}/{}): {:?}",
                String::from_utf8_lossy(needle),
                acks.len(),
                String::from_utf8_lossy(&acc)
            );
        };
        match frame {
            FrameKind::ResourceOutput { bytes, .. } => acc.extend_from_slice(&bytes),
            FrameKind::CommandResult { request_id, result } if acks.contains(&request_id) => {
                assert_eq!(result, CommandResult::Ok, "{acc:?}");
                seen += 1;
            }
            _ => {}
        }
    }
}

/// A burst of `INPUT_KEY` frames that lands in one socket read reaches the
/// PTY whole. Typed text (the desktop's `commitText`, an IME commit) is one
/// frame per scalar, so a 256-character line arrives as one buffered read,
/// four times the pane's 64 input credits. Past those the server must wait
/// for the pane to drain (ADR-0144); it used to drop everything past 64.
#[test]
fn a_buffered_input_key_burst_reaches_the_pty_whole() {
    const LETTERS: [(char, PhysicalKey); 5] = [
        ('a', PhysicalKey::A),
        ('b', PhysicalKey::B),
        ('c', PhysicalKey::C),
        ('d', PhysicalKey::D),
        ('e', PhysicalKey::E),
    ];
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let ((shutdown, server), mut stream) = seeded(&tmp, CommandBuilder::new("/bin/cat")).await;
        let pane = attach(&mut stream).await;

        let typed: String = (0..256).map(|i| LETTERS[i % LETTERS.len()].0).collect();
        let mut burst = Vec::new();
        for i in 0..typed.len() {
            let (c, k) = LETTERS[i % LETTERS.len()];
            burst.extend_from_slice(&phux_server_testkit::encode_frame(&key(&pane, c, k)));
        }
        let enter = FrameKind::InputKey {
            terminal_id: pane.clone(),
            event: named_key(PhysicalKey::Enter),
        };
        burst.extend_from_slice(&phux_server_testkit::encode_frame(&enter));
        tokio::io::AsyncWriteExt::write_all(&mut stream, &burst)
            .await
            .unwrap();

        // Inside `try_recv_typed`'s own per-read timeout, so a lost tail
        // reports what did arrive rather than a bare read timeout.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut acc = Vec::new();
        while !acc.windows(typed.len()).any(|w| w == typed.as_bytes()) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Ok(Some((_, frame))) = timeout(remaining, try_recv_typed(&mut stream)).await else {
                panic!(
                    "the {}-key burst did not reach the PTY whole; echoed {} bytes: {:?}",
                    typed.len(),
                    acc.len(),
                    String::from_utf8_lossy(&acc)
                );
            };
            if let FrameKind::ResourceOutput { bytes, .. } = frame {
                acc.extend_from_slice(&bytes);
            }
        }

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

/// ADR-0144: input to a pane whose program has stopped reading its
/// terminal waits instead of being dropped. Pastes fill the PTY, the writer
/// queue and every input credit; the server then stops reading this
/// connection, so the keystrokes typed after them wait in the socket. Once
/// the program reads again, every byte reaches it, in order.
#[test]
fn input_to_a_stalled_pane_waits_and_arrives_whole() {
    const PASTES: usize = 160;
    const PASTE_BYTES: usize = 1024;
    const LETTERS: [(char, PhysicalKey); 3] = [
        ('x', PhysicalKey::X),
        ('y', PhysicalKey::Y),
        ('z', PhysicalKey::Z),
    ];
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let ready = tmp.path().join("ready");
        let go = tmp.path().join("go");
        let out = tmp.path().join("out");
        // Raw and silent, so bytes reach `cat` unedited and nothing echoes;
        // `ready` says the tty is set. `cat` writes to a file, not the
        // terminal, so the output path cannot be what loses a byte.
        let script = format!(
            "stty raw -echo; : > '{ready}'; while [ ! -e '{go}' ]; do sleep 0.02; done; exec cat > '{out}'",
            ready = ready.display(),
            go = go.display(),
            out = out.display(),
        );
        let ((shutdown, server), mut stream) = seeded(&tmp, sh(&script)).await;
        let pane = attach(&mut stream).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while !ready.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the pane never set its tty raw"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut expected = Vec::new();
        let mut burst = Vec::new();
        for i in 0..PASTES {
            // Distinct per paste, so a lost or reordered one shows.
            let data = vec![b'a' + u8::try_from(i % 26).unwrap(); PASTE_BYTES];
            expected.extend_from_slice(&data);
            let paste = FrameKind::InputPaste {
                terminal_id: pane.clone(),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data,
                },
            };
            burst.extend_from_slice(&phux_server_testkit::encode_frame(&paste));
        }
        for i in 0..256 {
            let (c, k) = LETTERS[i % LETTERS.len()];
            expected.push(u8::try_from(c).unwrap());
            burst.extend_from_slice(&phux_server_testkit::encode_frame(&key(&pane, c, k)));
        }

        // Release the program only once the server has had to wait for the
        // pane to drain: the burst cannot finish writing before that.
        let waits_before = phux_terminal::perf::INPUT_CREDIT_WAITS.get();
        let release = tokio::task::spawn_local(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            while phux_terminal::perf::INPUT_CREDIT_WAITS.get() == waits_before {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the stalled pane never saturated"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            std::fs::write(&go, b"").unwrap();
        });
        tokio::io::AsyncWriteExt::write_all(&mut stream, &burst)
            .await
            .unwrap();
        release.await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let written = loop {
            let written = std::fs::read(&out).unwrap_or_default();
            if written.len() >= expected.len() || tokio::time::Instant::now() >= deadline {
                break written;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(
            written.len(),
            expected.len(),
            "bytes reaching the program after the stall"
        );
        assert!(
            written == expected,
            "input reached the program out of order"
        );

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

mod later;
