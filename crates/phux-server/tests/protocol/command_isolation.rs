//! Load-bearing wire acceptance for bounded background commands.
//!
//! These tests drive a real `ServerRuntime`, PTYs, the production connection
//! dispatcher, upload worker, transcriber process, and input lane. They pin the
//! properties that helper-only tests cannot: slow work does not stall control
//! or another Terminal, accepted bulk commands stay FIFO, refusals never start
//! a process, and connection teardown kills work and releases global admission.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use phux_protocol::ids::{FileUploadId, GroupId, InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, SpawnResult,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::{Instant, timeout};

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, await_command_result,
    join_after_shutdown, recv_typed, run_local, send_frame, spawn_server_with, wait_for_socket,
};

const SESSION: &str = "command-isolation";
static BULK_TEST_LOCK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

fn paste(bytes: &[u8]) -> InputEvent {
    InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Trusted,
        data: bytes.to_vec(),
    })
}

fn blocking_transcriber(started: &Path, release: &Path) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "label=$(cat \"$1\"); printf '%s %s\\n' \"$label\" \"$$\" >> \"$2\"; \
         while [ ! -e \"$3\" ]; do sleep 0.01; done; printf '%s\\n' \"$label\""
            .to_owned(),
        "sh".to_owned(),
        "{path}".to_owned(),
        started.display().to_string(),
        release.display().to_string(),
    ]
}

fn passthrough_transcriber() -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "test -s \"$1\" && cat \"$1\"".to_owned(),
        "sh".to_owned(),
        "{path}".to_owned(),
    ]
}

fn line_recording_command(marker: &Path) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "stty -echo; while IFS= read -r line; do printf '%s\\n' \"$line\" >> \"$1\"; done"
            .to_owned(),
        "sh".to_owned(),
        marker.display().to_string(),
    ]
}

async fn attach(stream: &mut UnixStream) -> ResourceId {
    send_frame(stream, &attach_by_name(SESSION)).await;
    loop {
        let (_, frame) = recv_typed(stream).await;
        if let FrameKind::Attached { snapshot, .. } = frame {
            return snapshot.resources[0].id.clone();
        }
    }
}

async fn spawn_terminal(stream: &mut UnixStream, request_id: u32, marker: &Path) -> ResourceId {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(line_recording_command(marker)),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        },
    )
    .await;
    loop {
        let (_, frame) = recv_typed(stream).await;
        if let FrameKind::ResourceSpawned {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return match result {
                SpawnResult::Ok(id) => id,
                other => panic!("SPAWN_RESOURCE failed: {other:?}"),
            };
        }
    }
}

async fn upload(
    stream: &mut UnixStream,
    terminal_id: &ResourceId,
    request_id: u32,
    upload_id: FileUploadId,
    bytes: &[u8],
) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::PutFile {
                upload_id,
                terminal_id: terminal_id.clone(),
                extension: "wav".to_owned(),
                offset: 0,
                data: bytes.to_vec(),
                final_chunk: true,
                sha256: Some(Sha256::digest(bytes).into()),
            },
        },
    )
    .await;
    match await_command_result(stream, request_id).await {
        CommandResult::OkWith(CommandValue::FileUpload(ack)) => {
            assert!(ack.path.is_some(), "final PUT_FILE must publish the upload");
        }
        other => panic!("PUT_FILE failed: {other:?}"),
    }
}

async fn wait_for_lines(path: &Path, count: usize) -> Vec<String> {
    timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await {
                let lines: Vec<_> = value.lines().map(str::to_owned).collect();
                if lines.len() >= count {
                    return lines;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("marker {} never reached {count} lines", path.display()))
}

fn transcript(result: CommandResult) -> String {
    let CommandResult::OkWith(CommandValue::Json(json)) = result else {
        panic!("TRANSCRIBE failed: {result:?}");
    };
    let value: serde_json::Value = serde_json::from_str(&json).expect("TRANSCRIBE JSON");
    value["text"].as_str().expect("transcript text").to_owned()
}

async fn next_command_result(stream: &mut UnixStream) -> (u32, CommandResult) {
    loop {
        let (_, frame) = recv_typed(stream).await;
        if let FrameKind::CommandResult { request_id, result } = frame {
            return (request_id, result);
        }
    }
}

async fn send_isolation_probes(stream: &mut UnixStream, terminal_id: ResourceId) {
    send_frame(stream, &FrameKind::Ping { nonce: 0x51_0a }).await;
    send_frame(
        stream,
        &FrameKind::Command {
            request_id: 11,
            command: Command::GetState {
                scope: phux_protocol::wire::frame::StateScope::Server,
            },
        },
    )
    .await;
    send_frame(
        stream,
        &FrameKind::Command {
            request_id: 12,
            command: Command::ApplyInput {
                operation_id: InputOperationId::new([0x12; 16]).unwrap(),
                terminal_id: terminal_id.clone(),
                events: vec![paste(b"apply-first\n")],
            },
        },
    )
    .await;
    send_frame(
        stream,
        &FrameKind::Command {
            request_id: 13,
            command: Command::RouteInput {
                terminal_id,
                event: paste(b"route-second\n"),
            },
        },
    )
    .await;
}

async fn await_isolation_probes(stream: &mut UnixStream) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut pong, mut state, mut apply, mut route) = (false, false, false, false);
    while !(pong && state && apply && route) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "control/input replies stalled behind TRANSCRIBE"
        );
        let (_, frame) = timeout(remaining, recv_typed(stream))
            .await
            .expect("connection stayed live while TRANSCRIBE was held");
        match frame {
            FrameKind::Pong { nonce: 0x51_0a } => pong = true,
            FrameKind::CommandResult { request_id: 10, .. } => {
                panic!("TRANSCRIBE completed before the marker release")
            }
            FrameKind::CommandResult {
                request_id: 11,
                result: CommandResult::OkWith(CommandValue::State(_)),
            } => state = true,
            FrameKind::CommandResult {
                request_id: 12,
                result: CommandResult::Ok,
            } => apply = true,
            FrameKind::CommandResult {
                request_id: 13,
                result: CommandResult::Ok,
            } => route = true,
            _ => {}
        }
    }
}

#[test]
fn held_transcribe_does_not_block_control_or_independent_terminal_input() {
    run_local(async {
        let _serial = BULK_TEST_LOCK.acquire().await.unwrap();
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let upload_dir = tmp.path().join("uploads");
        let started = tmp.path().join("transcriber-started");
        let release = tmp.path().join("release-transcriber");
        let input_marker = tmp.path().join("terminal-b-input");
        unsafe { std::env::set_var("PHUX_UPLOAD_DIR", &upload_dir) };

        let transcriber = blocking_transcriber(&started, &release);
        let (shutdown, server) = spawn_server_with(socket.clone(), Some(SESSION), move |cfg| {
            cfg.seed_with_pty = true;
            cfg.seed_command = Some(portable_pty::CommandBuilder::new("cat"));
            cfg.voice.transcriber = Some(transcriber);
            cfg.voice.timeout_secs = Some(15);
        });
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let transcribe_terminal = attach(&mut stream).await;
        let input_terminal = spawn_terminal(&mut stream, 2, &input_marker).await;
        let upload_id = FileUploadId::new([0x31; 16]).unwrap();
        upload(
            &mut stream,
            &transcribe_terminal,
            3,
            upload_id,
            b"held transcript",
        )
        .await;

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 10,
                command: Command::Transcribe {
                    upload_id,
                    terminal_id: transcribe_terminal,
                },
            },
        )
        .await;
        let started_lines = wait_for_lines(&started, 1).await;
        assert!(
            marker_entry(&started_lines, "held transcript").is_some(),
            "the transcriber handshake must carry its complete label and pid",
        );
        let held_since = Instant::now();

        send_isolation_probes(&mut stream, input_terminal).await;
        await_isolation_probes(&mut stream).await;

        let lines = wait_for_lines(&input_marker, 2).await;
        assert_eq!(
            &lines[..2],
            ["apply-first", "route-second"],
            "the independent Terminal must preserve ApplyInput -> RouteInput order",
        );
        let minimum_hold = Duration::from_secs(2);
        if let Some(remaining) = minimum_hold.checked_sub(held_since.elapsed()) {
            tokio::time::sleep(remaining).await;
        }
        tokio::fs::write(&release, b"go").await.unwrap();
        assert_eq!(
            transcript(await_command_result(&mut stream, 10).await),
            "held transcript"
        );

        unsafe { std::env::remove_var("PHUX_UPLOAD_DIR") };
        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn put_file_then_transcribe_is_fifo_and_replies_stay_correlated() {
    run_local(async {
        let _serial = BULK_TEST_LOCK.acquire().await.unwrap();
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        unsafe { std::env::set_var("PHUX_UPLOAD_DIR", tmp.path().join("uploads")) };
        let (shutdown, server) = spawn_server_with(socket.clone(), Some(SESSION), |cfg| {
            cfg.seed_with_pty = true;
            cfg.seed_command = Some(portable_pty::CommandBuilder::new("cat"));
            cfg.voice.transcriber = Some(passthrough_transcriber());
        });
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let terminal_id = attach(&mut stream).await;
        let upload_id = FileUploadId::new([0x42; 16]).unwrap();
        let bytes = b"fifo transcript";

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 20,
                command: Command::PutFile {
                    upload_id,
                    terminal_id: terminal_id.clone(),
                    extension: "wav".to_owned(),
                    offset: 0,
                    data: bytes.to_vec(),
                    final_chunk: true,
                    sha256: Some(Sha256::digest(bytes).into()),
                },
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 21,
                command: Command::Transcribe {
                    upload_id,
                    terminal_id,
                },
            },
        )
        .await;

        let (put_request, put_result) = next_command_result(&mut stream).await;
        assert_eq!(put_request, 20, "PUT_FILE must complete before TRANSCRIBE");
        match put_result {
            CommandResult::OkWith(CommandValue::FileUpload(ack)) => assert!(ack.path.is_some()),
            other => panic!("request 20 was not the PUT_FILE ack: {other:?}"),
        }
        let (transcribe_request, transcribe_result) = next_command_result(&mut stream).await;
        assert_eq!(transcribe_request, 21, "TRANSCRIBE reply correlation");
        assert_eq!(
            transcript(transcribe_result),
            "fifo transcript",
            "TRANSCRIBE must observe the preceding final PUT_FILE without an ack barrier",
        );

        unsafe { std::env::remove_var("PHUX_UPLOAD_DIR") };
        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn stalled_disk_upload_does_not_block_a_new_connection() {
    run_local(async {
        let _serial = BULK_TEST_LOCK.acquire().await.unwrap();
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let hold = tmp.path().join("upload-hold");
        std::fs::create_dir(&hold).unwrap();
        unsafe { std::env::set_var("PHUX_UPLOAD_DIR", tmp.path().join("uploads")) };
        unsafe { std::env::set_var("PHUX_TEST_UPLOAD_HOLD", &hold) };

        let (shutdown, server) = spawn_server_with(socket.clone(), Some(SESSION), |cfg| {
            cfg.seed_with_pty = true;
            cfg.seed_command = Some(portable_pty::CommandBuilder::new("cat"));
        });
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let terminal_id = attach(&mut stream).await;
        let upload_id = FileUploadId::new([0x5d; 16]).unwrap();
        let bytes = b"held disk";

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 30,
                command: Command::PutFile {
                    upload_id,
                    terminal_id,
                    extension: "wav".to_owned(),
                    offset: 0,
                    data: bytes.to_vec(),
                    final_chunk: true,
                    sha256: Some(Sha256::digest(bytes).into()),
                },
            },
        )
        .await;

        timeout(WIRE_RECV_TIMEOUT, async {
            while !hold.join("held").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("admitted upload worker must reach the disk-hold barrier");

        timeout(Duration::from_secs(5), async {
            let mut peer = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
            attach(&mut peer).await
        })
        .await
        .expect("HELLO+ATTACH must complete while an admitted upload worker is held");

        tokio::fs::write(hold.join("release"), b"").await.unwrap();
        match await_command_result(&mut stream, 30).await {
            CommandResult::OkWith(CommandValue::FileUpload(ack)) => {
                assert!(ack.path.is_some(), "held PUT_FILE must still publish");
            }
            other => panic!("held PUT_FILE failed: {other:?}"),
        }

        unsafe { std::env::remove_var("PHUX_TEST_UPLOAD_HOLD") };
        unsafe { std::env::remove_var("PHUX_UPLOAD_DIR") };
        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

fn marker_entry(lines: &[String], label: &str) -> Option<u32> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(label)?.strip_prefix(' ')?.parse().ok())
}

fn process_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_for_process_exit(pid: u32) {
    timeout(WIRE_RECV_TIMEOUT, async {
        while process_exists(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("transcriber process {pid} survived connection teardown"));
}

/// Drop leftover saturation clients and wait for their transcribers to die.
///
/// They share one Terminal with the replacement. Writing the release file
/// while they are still alive races every paste onto one `APPLY_INPUT` slot
/// (`another APPLY_INPUT operation is in flight for this terminal`).
async fn wait_for_leftover_transcribers_to_exit(clients: Vec<UnixStream>, started: &[String]) {
    let leftover_pids: Vec<u32> = (1_usize..4)
        .map(|index| {
            marker_entry(started, &format!("client-{index}"))
                .expect("saturated connection recorded a start pid")
        })
        .collect();
    drop(clients);
    for pid in leftover_pids {
        wait_for_process_exit(pid).await;
    }
}

async fn submit_transcribe(
    stream: &mut UnixStream,
    request_id: u32,
    upload_id: FileUploadId,
    terminal_id: &ResourceId,
) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::Transcribe {
                upload_id,
                terminal_id: terminal_id.clone(),
            },
        },
    )
    .await;
}

#[test]
fn saturation_refuses_without_starting_and_teardown_releases_global_capacity() {
    run_local(async {
        let _serial = BULK_TEST_LOCK.acquire().await.unwrap();
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let started = tmp.path().join("starts");
        let release = tmp.path().join("release");
        unsafe { std::env::set_var("PHUX_UPLOAD_DIR", tmp.path().join("uploads")) };
        let transcriber = blocking_transcriber(&started, &release);
        let (shutdown, server) = spawn_server_with(socket.clone(), Some(SESSION), move |cfg| {
            cfg.seed_with_pty = true;
            cfg.seed_command = Some(portable_pty::CommandBuilder::new("cat"));
            cfg.voice.transcriber = Some(transcriber);
            cfg.voice.timeout_secs = Some(15);
        });

        let mut setup = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let terminal_id = attach(&mut setup).await;
        let mut uploads = Vec::new();
        for index in 0_u8..5 {
            let upload_id = FileUploadId::new([index + 1; 16]).unwrap();
            upload(
                &mut setup,
                &terminal_id,
                10 + u32::from(index),
                upload_id,
                format!("client-{index}").as_bytes(),
            )
            .await;
            uploads.push(upload_id);
        }
        drop(setup);

        let mut clients = Vec::new();
        for _ in 0..4 {
            clients.push(wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await);
        }
        submit_transcribe(&mut clients[0], 100, uploads[0], &terminal_id).await;
        let first_lines = wait_for_lines(&started, 1).await;
        let doomed_pid = marker_entry(&first_lines, "client-0").expect("client-0 start pid");
        for request_id in 101..=104 {
            submit_transcribe(&mut clients[0], request_id, uploads[0], &terminal_id).await;
        }
        match await_command_result(&mut clients[0], 104).await {
            CommandResult::Error { code, .. } => assert_eq!(code, ErrorCode::ResourceExhausted),
            other => panic!("fifth per-connection job must be refused: {other:?}"),
        }

        for client_index in 1_usize..4 {
            for slot in 0_u32..4 {
                submit_transcribe(
                    &mut clients[client_index],
                    200 + u32::try_from(client_index).unwrap() * 10 + slot,
                    uploads[client_index],
                    &terminal_id,
                )
                .await;
            }
        }
        let lines = wait_for_lines(&started, 4).await;
        for index in 0..4 {
            assert!(
                marker_entry(&lines, &format!("client-{index}")).is_some(),
                "each saturated connection must have exactly one active transcriber",
            );
        }

        let mut replacement = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        submit_transcribe(&mut replacement, 500, uploads[4], &terminal_id).await;
        match await_command_result(&mut replacement, 500).await {
            CommandResult::Error { code, .. } => assert_eq!(code, ErrorCode::ResourceExhausted),
            other => panic!("global saturation must refuse request 500: {other:?}"),
        }
        let refused_lines = wait_for_lines(&started, 4).await;
        assert!(
            marker_entry(&refused_lines, "client-4").is_none(),
            "a globally refused command must not start its transcriber",
        );

        let doomed = clients.remove(0);
        drop(doomed);
        wait_for_process_exit(doomed_pid).await;
        submit_transcribe(&mut replacement, 501, uploads[4], &terminal_id).await;
        let released_lines = wait_for_lines(&started, 5).await;
        assert!(
            marker_entry(&released_lines, "client-4").is_some(),
            "teardown must release global admission for a replacement connection",
        );
        assert_eq!(
            released_lines
                .iter()
                .filter(|line| line.starts_with("client-0 "))
                .count(),
            1,
            "refused and queued client-0 work must never start after teardown",
        );

        wait_for_leftover_transcribers_to_exit(clients, &released_lines).await;
        tokio::fs::write(&release, b"go").await.unwrap();
        assert_eq!(
            transcript(await_command_result(&mut replacement, 501).await),
            "client-4"
        );
        unsafe { std::env::remove_var("PHUX_UPLOAD_DIR") };
        drop(replacement);
        join_after_shutdown(shutdown, server).await;
    });
}
