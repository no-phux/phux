//! PHA-406 D5: the typed `process` facet of `GET_TERMINAL_STATE`, the OSC-133
//! prompt machine, signal-aware exit, and `cwd_changed` events. Children block
//! on `read` until the test presses Enter, so nothing races the spawn reply.

use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_core::process::{ExitOutcome, PromptState, TerminalProcessState};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
    SpawnResult, ViewportInfo,
};
use phux_server::terminal_actor::{ProcessFacetRequest, TerminalActor};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, ServerHandles, Spawn, command, recv_until, recv_until_deadline,
    run_local, send_frame, spawn_resource, spawn_server, wait_for_socket,
};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::common::{named_key, sh};

/// Hang guard only.
const DEADLINE: Duration = Duration::from_secs(30);

/// A server with session `work` attached on the returned stream.
async fn server_and_client(tmp: &TempDir) -> (UnixStream, ServerHandles) {
    let socket_path = tmp.path().join("phux.sock");
    let server = spawn_server(socket_path.clone(), None);
    let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    let attach = FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::CreateIfMissing {
            name: "work".to_owned(),
            command: None,
            cwd: None,
        },
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    };
    send_frame(&mut stream, &attach).await;
    recv_until(&mut stream, |_, f| {
        matches!(f, FrameKind::BootstrapBegin { .. }).then_some(())
    })
    .await;
    (stream, server)
}

async fn spawn_pane(stream: &mut UnixStream, argv: &[&str], cwd: Option<&Path>) -> ResourceId {
    let spawn = Spawn {
        cwd: cwd.map(|dir| dir.to_string_lossy().into_owned()),
        ..Spawn::command(argv)
    };
    match spawn_resource(stream, 1, spawn).await {
        SpawnResult::Ok(id) => id,
        other => panic!("spawn failed: {other:?}"),
    }
}

async fn press_enter(stream: &mut UnixStream, pane: &ResourceId) {
    let enter = FrameKind::InputKey {
        terminal_id: pane.clone(),
        event: named_key(PhysicalKey::Enter),
    };
    send_frame(stream, &enter).await;
}

async fn get_state(stream: &mut UnixStream, request_id: u32, pane: &ResourceId) -> CommandResult {
    let get = Command::GetTerminalState {
        terminal_id: pane.clone(),
        include_scrollback: false,
        max_scrollback_lines: 0,
    };
    command(stream, request_id, get).await
}

/// Poll `GET_TERMINAL_STATE` until `done` holds for the typed process facet;
/// returns the whole document alongside it.
async fn poll_process(
    stream: &mut UnixStream,
    pane: &ResourceId,
    done: impl Fn(&TerminalProcessState) -> bool,
) -> (serde_json::Value, TerminalProcessState) {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    for request_id in 100.. {
        let CommandResult::OkWith(CommandValue::Json(json)) =
            get_state(stream, request_id, pane).await
        else {
            panic!("GET_TERMINAL_STATE on a live pane must answer OK_WITH(JSON)");
        };
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        let process: TerminalProcessState = serde_json::from_value(doc["process"].clone()).unwrap();
        if done(&process) {
            return (doc, process);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "last facet {process:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    unreachable!()
}

/// `RESOURCE_CLOSED` for `pane` as `(exit_status, signal)`.
async fn await_closed(stream: &mut UnixStream, pane: &ResourceId) -> (Option<i32>, Option<i32>) {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    recv_until_deadline(stream, deadline, |_, frame| match frame {
        FrameKind::ResourceClosed {
            terminal_id,
            exit_status,
            signal,
            ..
        } if &terminal_id == pane => Some((exit_status, signal)),
        _ => None,
    })
    .await
    .expect("RESOURCE_CLOSED")
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn unix_now_ms() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    u64::try_from(now.unwrap().as_millis()).unwrap()
}

/// The child is named by `(pid, start_ms)` with a real start time; the cwd is
/// the kernel's view of the spawn directory; and the foreground group is the
/// tty's (the child is a session leader) with argv0's basename only.
#[test]
fn process_facet_reports_child_foreground_and_cwd() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let (mut stream, _server) = server_and_client(&tmp).await;
        let before_ms = unix_now_ms();
        let pane = spawn_pane(&mut stream, &["/bin/sleep", "30"], Some(&work)).await;

        let (doc, process) = poll_process(&mut stream, &pane, |p| {
            p.foreground.as_ref().and_then(|fg| fg.name.as_deref()) == Some("sleep")
        })
        .await;
        assert_eq!(doc["schema_version"], 1, "{doc}");
        assert_eq!(doc["shell_state"]["state"], "unknown", "{doc}");
        let child = process.child.expect("child");
        let start_ms = child.start_ms.expect("start time");
        // Linux derives it from whole-second `btime`: allow a second each way.
        assert!(start_ms + 1_000 >= before_ms && start_ms <= unix_now_ms() + 1_000);
        assert_eq!(
            canonical(Path::new(&process.cwd.unwrap())),
            canonical(&work)
        );
        assert!(process.exit.is_none());
        let foreground = process.foreground.unwrap();
        assert_eq!(
            (foreground.pgid, foreground.start_ms),
            (child.pid, child.start_ms)
        );
    });
}

/// The prompt machine flips at each OSC-133 mark, and `D;3` records the exit
/// code.
#[test]
fn get_terminal_state_prompt_state_flips_at_osc133_marks() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, _server) = server_and_client(&tmp).await;
        let script = "printf '\\033]133;A\\007$ '; read _; \
                      printf '\\033]133;C\\007'; read _; \
                      printf '\\033]133;D;3\\007'; read _";
        let pane = spawn_pane(&mut stream, &["/bin/sh", "-c", script], None).await;
        poll_process(&mut stream, &pane, |p| {
            p.prompt.state == PromptState::AtPrompt
        })
        .await;
        press_enter(&mut stream, &pane).await;
        poll_process(&mut stream, &pane, |p| {
            p.prompt.state == PromptState::Running
        })
        .await;
        press_enter(&mut stream, &pane).await;
        let (_, p) = poll_process(&mut stream, &pane, |p| {
            p.prompt.state == PromptState::AtPrompt
        })
        .await;
        assert_eq!(p.prompt.last_exit_code, Some(3));
    });
}

/// A signal death is reported, not flattened: `RESOURCE_CLOSED` carries
/// `signal: Some(9)` beside `exit_status: None`. A non-retained pane is reaped
/// with its child, so its facet is `TERMINAL_NOT_FOUND`, not a stale document.
#[test]
fn exits_are_reported_on_resource_closed_and_reap_the_facet() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, _server) = server_and_client(&tmp).await;
        let killed = spawn_pane(&mut stream, &["/bin/sh", "-c", "read _; kill -9 $$"], None).await;
        press_enter(&mut stream, &killed).await;
        assert_eq!(await_closed(&mut stream, &killed).await, (None, Some(9)));

        let exited = spawn_pane(&mut stream, &["/bin/sh", "-c", "read _; exit 0"], None).await;
        press_enter(&mut stream, &exited).await;
        assert_eq!(await_closed(&mut stream, &exited).await, (Some(0), None));
        match get_state(&mut stream, 2, &exited).await {
            CommandResult::Error { code, .. } => assert_eq!(code, ErrorCode::TerminalNotFound),
            other => panic!("expected TERMINAL_NOT_FOUND after the reap, got {other:?}"),
        }
    });
}

/// The actor-level exit facet (the only way to read it for a non-retained
/// pane): the engine outcome and the facet both name the signal.
#[test]
fn signal_death_is_reported_in_the_exit_facet_not_flattened() {
    run_local(async {
        let mut bundle = TerminalActor::new_with_command(sh("kill -9 $$"), 40, 5).unwrap();
        let exit_rx = bundle.exit_notify.take().unwrap();
        let terminal = bundle.handle.terminal().unwrap().clone();
        let token = bundle.token.clone();
        tokio::task::spawn_local(bundle.actor.run());

        let outcome = timeout(DEADLINE, exit_rx).await.unwrap().unwrap();
        assert_eq!(outcome, ExitOutcome::signaled(9));
        let (reply, reply_rx) = oneshot::channel();
        terminal
            .process
            .send(ProcessFacetRequest { reply })
            .await
            .unwrap();
        let facet = timeout(DEADLINE, reply_rx).await.unwrap().unwrap();
        let exit = facet.exit.expect("exit facet recorded at EOF");
        assert_eq!(
            (exit.signal, exit.status, exit.reason.as_str()),
            (Some(9), None, "exited")
        );
        assert!(exit.exited_at_ms.is_some());
        assert!(facet.child.is_some());
        assert!(
            facet.foreground.is_none() && facet.cwd.is_none(),
            "no live query after exit"
        );
        token.cancel();
    });
}

/// `cwd_changed` events for `pane` seen within `window`.
async fn cwd_events(stream: &mut UnixStream, window: Duration) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
        && let Ok(frame) = timeout(remaining, phux_server_testkit::recv_typed(stream)).await
    {
        if let FrameKind::Event {
            event: AgentEvent::CwdChanged { cwd },
            ..
        } = frame.1
        {
            seen.push(canonical(Path::new(&cwd)));
        }
    }
    seen
}

/// A fresh pane announces its real starting directory exactly once (a
/// mid-session consumer has no other source), stays silent on further output
/// in the same directory, and announces a later `cd $HOME`. The seed is the
/// child's real cwd, not `$HOME`, so that `cd` is never swallowed.
#[test]
fn cwd_changed_announces_the_start_dir_once_then_real_moves() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    run_local(async move {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        assert_ne!(canonical(&work), canonical(&home));
        let (mut stream, _server) = server_and_client(&tmp).await;
        let script = "read _; printf 'one\\n'; read _; printf 'two\\n'; \
                      read _; cd \"$HOME\" && printf '\\033]133;D;0\\007'; read _";
        let pane = spawn_pane(&mut stream, &["/bin/sh", "-c", script], Some(&work)).await;
        let subscribe = Command::SubscribeResourceEvents {
            terminal_id: pane.clone(),
            event_types: Vec::new(),
        };
        assert_eq!(command(&mut stream, 2, subscribe).await, CommandResult::Ok);

        press_enter(&mut stream, &pane).await;
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + DEADLINE;
        while seen.is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no initial cwd_changed"
            );
            seen.extend(cwd_events(&mut stream, Duration::from_millis(200)).await);
        }
        press_enter(&mut stream, &pane).await;
        seen.extend(cwd_events(&mut stream, Duration::from_millis(1_500)).await);
        assert_eq!(
            seen,
            vec![canonical(&work)],
            "exactly one initial cwd_changed"
        );

        press_enter(&mut stream, &pane).await;
        while !seen.contains(&canonical(&home)) {
            assert!(
                tokio::time::Instant::now() < deadline + DEADLINE,
                "no cwd_changed to $HOME: {seen:?}"
            );
            seen.extend(cwd_events(&mut stream, Duration::from_millis(200)).await);
        }
    });
}
