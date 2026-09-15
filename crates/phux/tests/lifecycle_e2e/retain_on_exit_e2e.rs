//! Binary-level acceptance for retain on exit (ADR-0124, PHA-406 D2/D3): a
//! real `phux server`, a Terminal spawned with `retain_secs`, and a waiter
//! that arrives after the process has already exited. Without retention that
//! waiter reads `TERMINAL_NOT_FOUND` and cannot tell "exited 42" from "never
//! existed"; with it, the wait is a level read of retained state: subscribe,
//! then `GET_STATE`, on one connection.
//!
//! `#[ignore]`: it spawns a real server. Run via `just e2e` (or
//! `cargo test -p phux --test lifecycle_e2e retain_on_exit_e2e:: -- --ignored`).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    CloseReason, Command, CommandResult, CommandValue, FrameKind, ResourceLifecycle, SpawnResource,
    SpawnResult, StateScope,
};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);

/// A `phux server` child with a long-lived seed pane, killed on drop.
struct Server {
    process: common::ServerProcess,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Server {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir
            .path()
            .join(format!("retain-{}.sock", std::process::id()));
        let child = Process::new(PHUX)
            .args([
                "server",
                "--session",
                "work",
                "--seed-command",
                "exec sleep 600",
                "--socket",
            ])
            .arg(&socket)
            // A backstop under the drop kill (ADR-0063), as upgrade_e2e.
            .args(["--exit-after-idle", "600"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let server = Self {
            process: common::ServerProcess::from_child(child, socket.clone()),
            socket,
            _dir: dir,
        };
        let deadline = Instant::now() + DEADLINE;
        while !server.socket.exists() {
            assert!(
                Instant::now() < deadline,
                "the server never bound its socket"
            );
            std::thread::sleep(POLL);
        }
        server
    }
}

impl Server {
    fn pid(&mut self) -> u32 {
        self.process.child_mut().id()
    }
}

/// Pseudoterminal descriptors `pid` holds, or `None` where `lsof` cannot
/// say.
fn pty_fds(pid: u32) -> Option<usize> {
    let out = Process::new("lsof")
        .args(["-n", "-P", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.stdout.is_empty() {
        return None;
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    Some(
        listing
            .lines()
            .filter(|line| {
                line.contains("ptmx") || line.contains("/dev/pts/") || line.contains("/dev/ttys")
            })
            .count(),
    )
}

/// Wait until `pid` holds no more pseudoterminal descriptors than
/// `baseline` (ADR-0124: a retained pane lets go of its PTY). Nothing to
/// check where `lsof` is unavailable.
fn assert_ptys_released(pid: u32, baseline: Option<usize>) {
    let Some(baseline) = baseline else {
        return;
    };
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = pty_fds(pid).expect("lsof answered for the baseline");
        if held <= baseline {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "a retained pane still holds its PTY: {held} descriptors, baseline {baseline}"
        );
        std::thread::sleep(POLL);
    }
}

async fn connect(socket: &Path) -> Connection {
    tokio::time::timeout(DEADLINE, Connection::connect(socket))
        .await
        .expect("connect in time")
        .expect("HELLO")
}

async fn request(conn: &mut Connection, request_id: u32, command: Command) -> CommandResult {
    tokio::time::timeout(DEADLINE, conn.request(request_id, command))
        .await
        .expect("answered in time")
        .expect("request")
        .into_result_ignoring_interleaved()
}

async fn state(conn: &mut Connection, request_id: u32) -> SessionSnapshot {
    match request(
        conn,
        request_id,
        Command::GetState {
            scope: StateScope::Server,
        },
    )
    .await
    {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
        other => panic!("GET_STATE failed: {other:?}"),
    }
}

fn find<'a>(snapshot: &'a SessionSnapshot, pane: &ResourceId) -> Option<&'a ResourceInfo> {
    snapshot.resources.iter().find(|r| &r.id == pane)
}

const fn exited(info: &ResourceInfo) -> bool {
    matches!(info.lifecycle, ResourceLifecycle::Exited)
}

/// Spawn `exit 42` beside the seed pane, retained for ten minutes, and return
/// its id. The spawner connection is dropped before the process is known to
/// have exited: nobody is watching when it does.
async fn spawn_retained_exit(socket: &Path) -> ResourceId {
    let mut spawner = connect(socket).await;
    let seed = state(&mut spawner, 1)
        .await
        .resources
        .first()
        .map(|seed| seed.id.clone())
        .expect("the seed pane");
    spawner
        .send(&FrameKind::SpawnResource {
            request_id: 2,
            group: GroupId::new(1),
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "exit 42".to_owned(),
            ]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: Some(seed),
            agent_session: None,
            initial_size: None,
            resource: Some(Box::new(
                SpawnResource::default().with_retain_secs(Some(600)),
            )),
        })
        .await
        .expect("send SPAWN_RESOURCE");
    let pane = loop {
        let frame = tokio::time::timeout(DEADLINE, spawner.recv())
            .await
            .expect("RESOURCE_SPAWNED in time")
            .expect("recv");
        if let FrameKind::ResourceSpawned {
            request_id: 2,
            result,
        } = frame
        {
            match result {
                SpawnResult::Ok(id) => break id,
                other => panic!("SPAWN_RESOURCE failed: {other:?}"),
            }
        }
    };
    drop(spawner);
    pane
}

/// Poll `GET_STATE` from `conn` until `ready` holds.
async fn wait_for(
    conn: &mut Connection,
    first_request_id: u32,
    what: &str,
    ready: impl Fn(&SessionSnapshot) -> bool + Send + Sync,
) {
    let deadline = Instant::now() + DEADLINE;
    for request_id in first_request_id.. {
        if ready(&state(conn, request_id).await) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(POLL).await;
    }
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn spawn_retain_then_wait_exit_after_the_fact_reads_status() {
    let mut server = Server::start();
    let pid = server.pid();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        // The seed pane's PTY is the baseline a retained pane must return to
        // once its process has exited.
        let mut bystander = connect(&server.socket).await;
        wait_for(&mut bystander, 5, "the seed pane", |s| {
            !s.resources.is_empty()
        })
        .await;
        let baseline = pty_fds(pid);

        let pane = spawn_retained_exit(&server.socket).await;

        // The bystander makes sure the exit is already in the past.
        wait_for(&mut bystander, 100, "the pane to exit", |s| {
            find(s, &pane).is_some_and(exited)
        })
        .await;

        // The waiter arrives after the fact and runs the D2 wait: subscribe
        // to the Terminal, then read the level on the same connection.
        let mut waiter = connect(&server.socket).await;
        waiter
            .send(&FrameKind::SubscribeEvents {
                terminal: Some(pane.clone()),
                after_seq: None,
            })
            .await
            .expect("subscribe");
        let snapshot = state(&mut waiter, 1000).await;
        let info = find(&snapshot, &pane)
            .expect("a retained exit is still listed, not TERMINAL_NOT_FOUND");
        let exit = info.exit.expect("the exit facet");
        assert_eq!(exit.exit_status, Some(42), "the waiter reads the status");
        assert_eq!(exit.signal, None);
        assert_eq!(exit.reason, CloseReason::Exited);
        assert!(exit.retained_until_ms > exit.exited_at_ms);

        let CommandResult::OkWith(CommandValue::Json(json)) = request(
            &mut waiter,
            1001,
            Command::GetTerminalState {
                terminal_id: pane.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
        )
        .await
        else {
            panic!("GET_TERMINAL_STATE answers a retained pane");
        };
        let terminal_state: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(terminal_state["process"]["exit"]["status"], 42);
        assert_eq!(terminal_state["process"]["exit"]["reason"], "exited");

        // The retained pane let go of its PTY and still answers reads.
        assert_ptys_released(pid, baseline);
        let history = request(
            &mut waiter,
            1003,
            Command::GetScreen {
                terminal_id: pane.clone(),
                request_scrollback: Some(0),
                cells: false,
                format: 0,
            },
        )
        .await;
        assert!(matches!(history, CommandResult::OkWith(_)), "{history:?}");
        let attach = request(
            &mut waiter,
            1004,
            Command::AttachResource {
                terminal_id: pane.clone(),
                role_policy: None,
            },
        )
        .await;
        assert!(matches!(attach, CommandResult::Ok), "{attach:?}");

        // The purge is an explicit kill; afterwards the id is gone.
        let kill = request(
            &mut waiter,
            1002,
            Command::KillResource {
                terminal_id: pane.clone(),
                operation_id: None,
            },
        )
        .await;
        assert!(matches!(kill, CommandResult::Ok), "{kill:?}");
        drop(waiter);
        wait_for(&mut bystander, 2000, "the purge", |s| {
            find(s, &pane).is_none()
        })
        .await;
    });
}
