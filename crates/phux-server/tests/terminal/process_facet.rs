//! PHA-406 D5: the typed `process` facet of `GET_TERMINAL_STATE`, the
//! OSC-133 prompt machine, the pid generation, and signal-aware exit.
//!
//! The server-level tests drive a real server over its socket and spawn real
//! PTY children through `SPAWN_RESOURCE`. Each child blocks on `read` until
//! the test releases it with an Enter key, so no output or exit can race the
//! spawn reply. The actor-level test drives a bare `TerminalActor`. It is
//! the only way to read the exit facet of a non-retained pane, because the
//! server reaps such a pane the moment its child exits.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_core::process::{ExitOutcome, PromptState, TerminalProcessState};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
    SpawnResult, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_RESOURCE_CLOSED, TYPE_RESOURCE_SPAWNED,
    ViewportInfo,
};
use phux_server::DEFAULT_GROUP_ID;
use phux_server::terminal_actor::{ProcessFacetRequest, TerminalActor, TerminalHandle};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, await_command_result, join_after_shutdown,
    recv_typed, run_local, send_frame, spawn_server, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::time::timeout;

/// One generous bound for every wait here. Nothing measures latency; the
/// bound only turns a hung child into a failure instead of a wedged run.
const DEADLINE: Duration = Duration::from_secs(30);

/// Pause between `GET_TERMINAL_STATE` polls while waiting for a mark.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

type ServerHandle = tokio::task::JoinHandle<Result<(), phux_server::ServerError>>;

/// Start a server, attach one client (creating session `work`), and drain
/// the attach handshake so the next frames are ours.
async fn server_and_client(tmp: &TempDir) -> (UnixStream, oneshot::Sender<()>, ServerHandle) {
    let socket_path = tmp.path().join("phux.sock");
    let (shutdown_tx, server) = spawn_server(socket_path.clone(), None);
    let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::CreateIfMissing {
                name: "work".to_owned(),
                command: None,
                cwd: None,
            },
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        },
    )
    .await;
    let (type_byte, _) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED);
    let (type_byte, _) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);
    (stream, shutdown_tx, server)
}

/// `SPAWN_RESOURCE` a Terminal running `argv` in `cwd`; returns its id.
async fn spawn_pane(
    stream: &mut UnixStream,
    request_id: u32,
    argv: &[&str],
    cwd: Option<&Path>,
) -> ResourceId {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: DEFAULT_GROUP_ID,
            command: Some(argv.iter().map(|a| (*a).to_owned()).collect()),
            cwd: cwd.map(|dir| dir.to_string_lossy().into_owned()),
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
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_RESOURCE_SPAWNED {
            continue;
        }
        if let FrameKind::ResourceSpawned {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            match result {
                SpawnResult::Ok(id) => return id,
                other => panic!("spawn failed: {other:?}"),
            }
        }
    }
    panic!("timed out waiting for RESOURCE_SPAWNED {request_id}");
}

/// Press Enter in `pane`: releases one `read _` in the child.
async fn press_enter(stream: &mut UnixStream, pane: &ResourceId) {
    send_frame(
        stream,
        &FrameKind::InputKey {
            terminal_id: pane.clone(),
            event: KeyEvent {
                action: KeyAction::Press,
                key: PhysicalKey::Enter,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: None,
                unshifted_codepoint: None,
            },
        },
    )
    .await;
}

/// Issue `GET_TERMINAL_STATE` and return the raw result.
async fn get_state(stream: &mut UnixStream, request_id: u32, pane: &ResourceId) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetTerminalState {
                terminal_id: pane.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

/// `GET_TERMINAL_STATE` on a live pane, parsed: the whole document and its
/// typed `process` object.
async fn get_process(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &ResourceId,
) -> (serde_json::Value, TerminalProcessState) {
    let CommandResult::OkWith(CommandValue::Json(json)) = get_state(stream, request_id, pane).await
    else {
        panic!("GET_TERMINAL_STATE on a live pane must answer OK_WITH(JSON)");
    };
    let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let process: TerminalProcessState =
        serde_json::from_value(doc["process"].clone()).expect("typed process facet");
    (doc, process)
}

/// Poll `GET_TERMINAL_STATE` until `done` holds for the process facet.
async fn poll_process(
    stream: &mut UnixStream,
    first_request_id: u32,
    pane: &ResourceId,
    what: &str,
    done: impl Fn(&TerminalProcessState) -> bool,
) -> TerminalProcessState {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut request_id = first_request_id;
    loop {
        let (_, process) = get_process(stream, request_id, pane).await;
        if done(&process) {
            return process;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; last facet {process:?}",
        );
        request_id += 1;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Drain until `RESOURCE_CLOSED` for `pane`; returns its
/// `(exit_status, signal)`.
async fn await_closed(stream: &mut UnixStream, pane: &ResourceId) -> (Option<i32>, Option<i32>) {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_RESOURCE_CLOSED {
            continue;
        }
        if let FrameKind::ResourceClosed {
            terminal_id,
            exit_status,
            signal,
            ..
        } = frame
            && &terminal_id == pane
        {
            return (exit_status, signal);
        }
    }
    panic!("timed out waiting for RESOURCE_CLOSED for {pane:?}");
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn unix_now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
    )
    .expect("ms fits u64")
}

/// The actor's facet reply, read over the Terminal facet channel.
async fn query_facet(terminal: &TerminalHandle) -> TerminalProcessState {
    let (reply, reply_rx) = oneshot::channel();
    terminal
        .process
        .send(ProcessFacetRequest { reply })
        .await
        .expect("actor accepts facet requests");
    timeout(DEADLINE, reply_rx)
        .await
        .expect("facet reply within deadline")
        .expect("actor answered")
}

/// The child is the PTY's own process, named by `(pid, start_ms)` with a
/// real Unix-ms start time, and the cwd is the kernel's view of the spawn
/// directory.
#[test]
fn get_terminal_state_reports_child_pid_start_and_cwd() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let (mut stream, shutdown_tx, server) = server_and_client(&tmp).await;
        let before_ms = unix_now_ms();
        let pane = spawn_pane(&mut stream, 1, &["/bin/sh", "-c", "read _"], Some(&work)).await;

        let (doc, process) = get_process(&mut stream, 2, &pane).await;
        assert_eq!(doc["schema_version"], 1, "versioned document: {doc}");
        let child = process.child.expect("the PTY child is reported");
        assert!(child.pid > 0);
        let start_ms = child.start_ms.expect("start time is queryable");
        // Linux derives it from whole-second `btime`, so allow one second
        // either side of the spawn.
        assert!(
            start_ms + 1_000 >= before_ms && start_ms <= unix_now_ms() + 1_000,
            "start_ms {start_ms} is not the spawn time (spawned after {before_ms})",
        );
        let cwd = process.cwd.expect("cwd is queryable");
        assert_eq!(canonical(Path::new(&cwd)), canonical(&work));
        assert!(process.exit.is_none(), "a live child has no exit facet");
        assert_eq!(
            doc["shell_state"]["state"], "unknown",
            "shell_state mirrors the prompt facet",
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// The prompt machine flips at each OSC-133 mark the pane prints, and `D`
/// carries its exit code into `last_exit_code`.
#[test]
fn get_terminal_state_prompt_state_flips_at_osc133_marks() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server) = server_and_client(&tmp).await;
        let script = "printf '\\033]133;A\\007$ '; read _; \
                      printf '\\033]133;C\\007'; read _; \
                      printf '\\033]133;D;3\\007'; read _";
        let pane = spawn_pane(&mut stream, 1, &["/bin/sh", "-c", script], None).await;

        poll_process(&mut stream, 100, &pane, "at_prompt after A", |p| {
            p.prompt.state == PromptState::AtPrompt
        })
        .await;

        press_enter(&mut stream, &pane).await;
        poll_process(&mut stream, 200, &pane, "running after C", |p| {
            p.prompt.state == PromptState::Running
        })
        .await;

        press_enter(&mut stream, &pane).await;
        let process = poll_process(&mut stream, 300, &pane, "at_prompt after D;3", |p| {
            p.prompt.state == PromptState::AtPrompt
        })
        .await;
        assert_eq!(process.prompt.last_exit_code, Some(3));

        drop(stream);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// The foreground group is the tty's, and its name is argv0's basename
/// only: no directory and no arguments leave the server.
#[test]
fn foreground_is_the_tty_pgid_basename_only() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server) = server_and_client(&tmp).await;
        let pane = spawn_pane(&mut stream, 1, &["/bin/sleep", "30"], None).await;

        let process = poll_process(&mut stream, 2, &pane, "a foreground group", |p| {
            p.foreground.is_some()
        })
        .await;
        let child = process.child.expect("child");
        let foreground = process.foreground.expect("foreground");
        // The child is a session leader (setsid + TIOCSCTTY), so with no job
        // control it is the tty's foreground group.
        assert_eq!(foreground.pgid, child.pid);
        assert_eq!(
            foreground.start_ms, child.start_ms,
            "same process, same start"
        );
        assert_eq!(foreground.name.as_deref(), Some("sleep"), "basename only");

        drop(stream);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// A death by signal is reported on the frame, not flattened:
/// `RESOURCE_CLOSED` carries `signal: Some(9)` beside `exit_status: None`,
/// which is what an older consumer that ignores field 4 still reads.
#[test]
fn resource_closed_carries_signal_9_for_sigkill() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server) = server_and_client(&tmp).await;
        let pane = spawn_pane(
            &mut stream,
            1,
            &["/bin/sh", "-c", "read _; kill -9 $$"],
            None,
        )
        .await;
        press_enter(&mut stream, &pane).await;
        assert_eq!(await_closed(&mut stream, &pane).await, (None, Some(9)));

        drop(stream);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// A non-retained pane is reaped when its child exits, so its facet is
/// gone with it: `TERMINAL_NOT_FOUND`, not a stale document.
#[test]
fn process_facet_is_null_after_exit_of_a_non_retained_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server) = server_and_client(&tmp).await;
        let pane = spawn_pane(&mut stream, 1, &["/bin/sh", "-c", "read _; exit 0"], None).await;
        press_enter(&mut stream, &pane).await;
        assert_eq!(await_closed(&mut stream, &pane).await, (Some(0), None));

        match get_state(&mut stream, 2, &pane).await {
            CommandResult::Error { code, .. } => assert_eq!(code, ErrorCode::TerminalNotFound),
            other => panic!("expected TERMINAL_NOT_FOUND after the reap, got {other:?}"),
        }

        drop(stream);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// Signal deaths are no longer flattened. The engine's exit outcome and the
/// exit facet it serves both name the signal, and `status` stays empty.
#[test]
fn signal_death_is_reported_in_the_exit_facet_not_flattened() {
    run_local(async {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "kill -9 $$"]);
        let mut bundle = TerminalActor::new_with_command(cmd, 40, 5).expect("spawn");
        let exit_rx = bundle.exit_notify.take().expect("exit notify");
        let terminal = bundle.handle.terminal().expect("terminal facet").clone();
        let token = bundle.token.clone();
        tokio::task::spawn_local(bundle.actor.run());

        let outcome = timeout(DEADLINE, exit_rx)
            .await
            .expect("exit within deadline")
            .expect("exit notify fires");
        assert_eq!(outcome, ExitOutcome::signaled(9));

        // The actor stays alive after EOF for late reads; the facet is there.
        let facet = query_facet(&terminal).await;
        let exit = facet.exit.expect("exit facet recorded at EOF");
        assert_eq!(exit.signal, Some(9));
        assert_eq!(exit.status, None);
        assert_eq!(exit.reason, "exited");
        assert!(exit.exited_at_ms.is_some());
        assert!(facet.child.is_some(), "the child still names the process");
        assert!(facet.foreground.is_none(), "no live query after exit");
        assert!(facet.cwd.is_none(), "no live query after exit");

        token.cancel();
    });
}

/// The cwd seed is the child's real starting directory, not `$HOME`. A
/// shell that starts elsewhere and `cd`s home must announce the change.
/// Under the old `$HOME` seed that event was swallowed, or preceded by a
/// spurious one for the directory the shell never left.
#[test]
fn cwd_changed_fires_when_a_shell_started_elsewhere_cds_home() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return; // no home to cd to
    };
    run_local(async move {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        assert_ne!(canonical(&work), canonical(&home));
        let (mut stream, shutdown_tx, server) = server_and_client(&tmp).await;
        let script = "read _; cd \"$HOME\" && printf '\\033]133;D;0\\007'; read _";
        let pane = spawn_pane(&mut stream, 1, &["/bin/sh", "-c", script], Some(&work)).await;

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 2,
                command: Command::SubscribeResourceEvents {
                    terminal_id: pane.clone(),
                    event_types: Vec::new(),
                },
            },
        )
        .await;
        assert_eq!(
            await_command_result(&mut stream, 2).await,
            CommandResult::Ok
        );
        press_enter(&mut stream, &pane).await;

        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + DEADLINE;
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or_else(|| panic!("no cwd_changed to $HOME; saw {seen:?}"));
            let Ok((_, frame)) = timeout(remaining, recv_typed(&mut stream)).await else {
                panic!("no cwd_changed to $HOME; saw {seen:?}");
            };
            let FrameKind::Event {
                event: AgentEvent::CwdChanged { cwd },
                ..
            } = frame
            else {
                continue;
            };
            let cwd = canonical(Path::new(&cwd));
            seen.push(cwd.clone());
            if cwd == canonical(&home) {
                break;
            }
        }
        // The starting directory may be announced once (the first
        // observation always is, if it lands before the `cd`), never twice.
        assert!(
            seen.iter().filter(|cwd| **cwd == canonical(&work)).count() <= 1,
            "the spawn directory is announced at most once: {seen:?}",
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// Collect every `cwd_changed` for the subscribed pane within `window`.
async fn collect_cwd_events(stream: &mut UnixStream, window: Duration) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        let Ok((_, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if let FrameKind::Event {
            event: AgentEvent::CwdChanged { cwd },
            ..
        } = frame
        {
            seen.push(canonical(Path::new(&cwd)));
        }
    }
    seen
}

/// A freshly spawned pane announces its real starting directory exactly
/// once, even though that directory is also the seed. A consumer that
/// learns of a pane mid-session (a TUI split) has no other source for it.
/// Later output bursts in the same directory stay silent.
#[test]
fn a_fresh_pane_emits_exactly_one_initial_cwd_changed() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let (mut stream, shutdown_tx, server) = server_and_client(&tmp).await;
        let script = "read _; printf 'one\\n'; read _; printf 'two\\n'; read _";
        let pane = spawn_pane(&mut stream, 1, &["/bin/sh", "-c", script], Some(&work)).await;

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 2,
                command: Command::SubscribeResourceEvents {
                    terminal_id: pane.clone(),
                    event_types: Vec::new(),
                },
            },
        )
        .await;
        assert_eq!(
            await_command_result(&mut stream, 2).await,
            CommandResult::Ok
        );

        // First burst: the settle re-query is this pane's first observation.
        press_enter(&mut stream, &pane).await;
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + DEADLINE;
        while seen.is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no initial cwd_changed"
            );
            seen.extend(collect_cwd_events(&mut stream, Duration::from_millis(200)).await);
        }
        // Second burst in the same directory: deduplicated, no event.
        press_enter(&mut stream, &pane).await;
        seen.extend(collect_cwd_events(&mut stream, Duration::from_millis(1_500)).await);
        assert_eq!(
            seen,
            vec![canonical(&work)],
            "exactly one initial cwd_changed"
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server).await;
    });
}
