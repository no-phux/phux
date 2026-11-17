//! Real-server history and generation regressions retained after retiring their C wrapper.

use super::*;
use phux_client_core::history::HistoryCacheConfig;
use phux_client_runtime::control::ControlPlane;
use phux_protocol::BootstrapProfile;
use phux_server::LaunchCommand;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, join_after_shutdown, recv_typed_before,
    spawn_server_with_seed_cmd, wait_for_raw_socket,
};

fn plane(retained_rows: usize) -> ControlPlane {
    ControlPlane::new(ControlOptions {
        attach: Some(AttachTarget::ByName("native-progressive".to_owned())),
        viewport: (200, 3),
        scrollback_lines: 10_000,
        ..ControlOptions::default()
    })
    .with_history_config(HistoryCacheConfig {
        max_materialized_rows: retained_rows,
        prefetch_rows: retained_rows + 1,
        ..HistoryCacheConfig::default()
    })
}

async fn flush(plane: &mut ControlPlane, socket: &mut UnixStream) {
    for bytes in plane.take_outbound() {
        socket.write_all(&bytes).await.unwrap();
    }
}

#[derive(Default)]
struct Progress {
    pages: usize,
    rows: usize,
    finished: bool,
    live: bool,
}

impl Progress {
    fn observe(&mut self, frame: &FrameKind) {
        match frame {
            FrameKind::HelloOk {
                selected_profile, ..
            } => assert!(matches!(
                selected_profile,
                Some(BootstrapProfile::NativeState { .. })
            )),
            FrameKind::HistoryPage {
                rows, next_cursor, ..
            } => {
                self.pages += 1;
                self.rows += *rows as usize;
                self.finished = next_cursor.is_none();
            }
            FrameKind::HistoryRejected { reason, .. } => {
                panic!("cooperative history progress rejected: {reason:?}");
            }
            FrameKind::ResourceOutput { .. } if self.pages != 0 && !self.finished => {
                self.live = true;
            }
            _ => {}
        }
    }
}

async fn history_flow(
    command: LaunchCommand,
    markers: Option<(&std::path::Path, &std::path::Path)>,
) -> Progress {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("history.sock");
    let (shutdown, server) =
        spawn_server_with_seed_cmd(path.clone(), "native-progressive", command);
    let mut socket = wait_for_raw_socket(&path, SOCKET_CONNECT_DEADLINE).await;
    if let Some((ready, _)) = markers {
        wait_until("seed history", || ready.exists()).await;
    }
    let mut plane = plane(if markers.is_some() { 1 } else { 16 });
    plane.connection_opened();
    flush(&mut plane, &mut socket).await;
    let mut progress = Progress::default();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while !progress.finished {
        let (_, frame) = recv_typed_before(&mut socket, deadline).await.unwrap();
        progress.observe(&frame);
        plane.feed(frame).unwrap();
        if progress.pages == 1
            && !progress.finished
            && let Some((_, live)) = markers
        {
            std::fs::write(live, []).unwrap();
            while !progress.live {
                let (_, frame) = recv_typed_before(&mut socket, deadline).await.unwrap();
                progress.observe(&frame);
                plane.feed(frame).unwrap();
            }
        }
        flush(&mut plane, &mut socket).await;
    }
    assert_eq!(plane.status(), Status::Attached);
    drop((plane, socket));
    join_after_shutdown(shutdown, server).await;
    progress
}

#[test]
fn native_history_reaches_finish_with_zero_and_tiny_retention_and_interleaved_output() {
    run_local(async {
        let empty = history_flow(LaunchCommand::new("/bin/cat"), None).await;
        assert_eq!((empty.pages, empty.rows), (1, 0));

        let dir = TempDir::new().unwrap();
        let ready = dir.path().join("ready");
        let live = dir.path().join("live");
        let mut command = LaunchCommand::new("/bin/sh");
        command.args(["-c", &format!(
            "i=0; while [ $i -lt 3000 ]; do printf 'connection-history-%04d\\r\\n' \"$i\"; i=$((i+1)); done; \
             : > '{}'; while [ ! -e '{}' ]; do sleep 0.001; done; printf 'connection-live\\r\\n'; \
             while :; do sleep 1; done",
            ready.display(), live.display()
        )]);
        let history = history_flow(command, Some((&ready, &live))).await;
        assert!(history.pages >= 3, "two pages plus FINISH");
        assert!(history.rows > 1);
        assert!(history.live);
    });
}

#[test]
fn native_resize_after_late_server_start_retires_the_old_generation() {
    run_local(async {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("retry.sock");
        assert!(UnixStream::connect(&path).await.is_err());
        let mut command = LaunchCommand::new("/bin/sh");
        command.args([
            "-c",
            "while :; do printf 'retry-generation\\r\\n'; sleep 0.05; done",
        ]);
        let (shutdown, server) =
            spawn_server_with_seed_cmd(path.clone(), "native-progressive", command);
        let mut socket = wait_for_raw_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        let mut plane = plane(50_000);
        plane.connection_opened();
        flush(&mut plane, &mut socket).await;
        let mut initial = None;
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        loop {
            let (_, frame) = recv_typed_before(&mut socket, deadline).await.unwrap();
            if let FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                ..
            } = &frame
            {
                initial.get_or_insert((*stream_id, *bootstrap_id));
            }
            let ready = matches!(frame, FrameKind::AttachReady { .. });
            plane.feed(frame).unwrap();
            flush(&mut plane, &mut socket).await;
            if ready {
                break;
            }
        }
        let initial = initial.unwrap();
        let terminal = ResourceId::local(1);
        assert_eq!(
            plane.resize_terminal(&terminal, 120, 40),
            phux_client_runtime::control::TerminalResizeOutcome::Queued,
        );
        flush(&mut plane, &mut socket).await;
        let mut retired = false;
        loop {
            let (_, frame) = recv_typed_before(&mut socket, deadline).await.unwrap();
            match &frame {
                FrameKind::BootstrapTombstone {
                    stream_id,
                    bootstrap_id,
                    reason,
                    ..
                } if (*stream_id, *bootstrap_id) == initial => {
                    assert_eq!(*reason, phux_protocol::wire::frame::TombstoneReason::Resize);
                    retired = true;
                }
                FrameKind::BootstrapBegin { bootstrap_id, .. } if *bootstrap_id != initial.1 => {
                    assert!(retired, "replacement BEGIN follows the tombstone");
                }
                _ => {}
            }
            plane.feed(frame).unwrap();
            flush(&mut plane, &mut socket).await;
            if plane.publication().acquire(&terminal).is_some_and(|frame| {
                (frame.cols, frame.rows) == (120, 40)
                    && frame.bootstrap_id != initial.1.get()
                    && frame.text().contains("retry-generation")
            }) {
                break;
            }
        }
        let before = plane.publication().acquire(&terminal).unwrap();
        // A retired generation must not contaminate the replacement, whether
        // the frontend surfaces the benign-drop error or silently skips it.
        let _ = plane.feed(FrameKind::ResourceOutput {
            terminal_id: terminal.clone(),
            stream_id: initial.0,
            bootstrap_id: initial.1,
            seq: 1,
            bytes: b"genuinely stale".as_slice().into(),
        });
        let after = plane.publication().acquire(&terminal).unwrap();
        assert_eq!(after.bootstrap_id, before.bootstrap_id);
        assert_eq!(after.text(), before.text());
        assert_eq!(plane.status(), Status::Attached);
        drop((plane, socket));
        join_after_shutdown(shutdown, server).await;
    });
}
