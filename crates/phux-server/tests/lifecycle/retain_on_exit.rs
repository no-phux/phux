//! ADR-0124 retain on exit, through the production frame loop: a retained
//! Terminal's exit is a facet, and its close is a purge.
//!
//! The spawning connection (`owner`) is subscribed to every pane it spawns,
//! so that is where `RESOURCE_CLOSED` arrives. Reads and commands go over a
//! second connection (`probe`) and events over a third (`events`), so a
//! command reply never races a close frame on one socket. Each pane blocks on
//! `read` until the test releases it, so no exit races its own spawn reply.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::ids::{GroupId, InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, CommandValue, ControlAction, ErrorCode,
    FrameKind, ResourceLifecycle, SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY_PREFIX, Scope,
    SpawnResource, SpawnResult, StateScope, TerminalSignal,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};
use phux_server::ServerConfig;
use phux_server::state::RetainPolicy;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, await_command_result,
    join_after_shutdown, recv_typed, run_local, send_frame, spawn_server_with, wait_for_socket,
};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::{Instant, timeout};

/// The pre-seeded session the owner attaches to.
const SESSION: &str = "demo";
/// Bound on every event-driven wait. A hang guard, never a timing assertion.
const DEADLINE: Duration = Duration::from_secs(10);
/// How long the owner must stay free of a `RESOURCE_CLOSED` that must not come.
const QUIET: Duration = Duration::from_millis(300);

type ServerTask = tokio::task::JoinHandle<Result<(), phux_server::ServerError>>;

/// One `RESOURCE_CLOSED`, as the owner received it.
#[derive(Debug, PartialEq, Eq)]
struct Closed {
    terminal_id: ResourceId,
    exit_status: Option<i32>,
    reason: CloseReason,
    signal: Option<i32>,
}

struct Harness {
    owner: UnixStream,
    probe: UnixStream,
    events: UnixStream,
    next_request: u32,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: ServerTask,
    _tmp: TempDir,
}

impl Harness {
    async fn start(configure: impl FnOnce(&mut ServerConfig)) -> Self {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some(SESSION), configure);
        let mut owner = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut owner, &attach_by_name(SESSION)).await;
        let probe = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let mut events = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut events,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: None,
            },
        )
        .await;
        // Frames are processed in order, so once this answers the event
        // subscription is installed.
        send_frame(
            &mut events,
            &FrameKind::Command {
                request_id: 1,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        let _ = timeout(WIRE_RECV_TIMEOUT, await_command_result(&mut events, 1))
            .await
            .expect("the subscription barrier is answered");
        Self {
            owner,
            probe,
            events,
            next_request: 100,
            shutdown,
            server,
            _tmp: tmp,
        }
    }

    async fn stop(self) {
        drop(self.owner);
        drop(self.probe);
        drop(self.events);
        join_after_shutdown(self.shutdown, self.server).await;
    }

    const fn request_id(&mut self) -> u32 {
        self.next_request += 1;
        self.next_request
    }

    /// Spawn `sh -c script` from the owner, retained for `retain` seconds
    /// when `Some`, in `beside`'s window when given.
    async fn spawn(
        &mut self,
        script: &str,
        retain: Option<u32>,
        beside: Option<&ResourceId>,
    ) -> ResourceId {
        let resource =
            retain.map(|secs| Box::new(SpawnResource::default().with_retain_secs(Some(secs))));
        let command = Some(vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            script.to_owned(),
        ]);
        self.send_spawn(command, beside.cloned(), resource).await
    }

    /// Spawn an `AgentSession` bound to `parent`.
    async fn spawn_child(&mut self, parent: &ResourceId) -> ResourceId {
        let resource = Box::new(SpawnResource::agent_session(parent.clone(), "claude"));
        self.send_spawn(None, None, Some(resource)).await
    }

    async fn send_spawn(
        &mut self,
        command: Option<Vec<String>>,
        owner_terminal: Option<ResourceId>,
        resource: Option<Box<SpawnResource>>,
    ) -> ResourceId {
        let request_id = self.request_id();
        send_frame(
            &mut self.owner,
            &FrameKind::SpawnResource {
                request_id,
                group: GroupId::new(1),
                command,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal,
                agent_session: None,
                initial_size: None,
                resource,
            },
        )
        .await;
        loop {
            let (_, frame) = timeout(DEADLINE, recv_typed(&mut self.owner))
                .await
                .expect("RESOURCE_SPAWNED arrives");
            if let FrameKind::ResourceSpawned {
                request_id: got,
                result,
            } = frame
                && got == request_id
            {
                match result {
                    SpawnResult::Ok(id) => return id,
                    other => panic!("SPAWN_RESOURCE failed: {other:?}"),
                }
            }
        }
    }

    /// Let a pane blocked on `read` run to its exit.
    async fn release(&mut self, pane: &ResourceId) {
        send_frame(
            &mut self.owner,
            &FrameKind::InputKey {
                terminal_id: pane.clone(),
                event: key(PhysicalKey::Enter, None),
            },
        )
        .await;
    }

    async fn command(&mut self, command: Command) -> CommandResult {
        let request_id = self.request_id();
        send_frame(
            &mut self.probe,
            &FrameKind::Command {
                request_id,
                command,
            },
        )
        .await;
        timeout(
            WIRE_RECV_TIMEOUT,
            await_command_result(&mut self.probe, request_id),
        )
        .await
        .expect("the command is answered")
    }

    async fn state(&mut self) -> SessionSnapshot {
        match self
            .command(Command::GetState {
                scope: StateScope::Server,
            })
            .await
        {
            CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
            other => panic!("GET_STATE failed: {other:?}"),
        }
    }

    /// Poll `GET_STATE` until `ready` holds.
    async fn wait_for_state(
        &mut self,
        what: &str,
        ready: impl Fn(&SessionSnapshot) -> bool,
    ) -> SessionSnapshot {
        let end = Instant::now() + DEADLINE;
        loop {
            let snapshot = self.state().await;
            if ready(&snapshot) {
                return snapshot;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Wait until `pane` is listed as `EXITED`, and return its entry.
    async fn wait_exited(&mut self, pane: &ResourceId) -> ResourceInfo {
        let snapshot = self
            .wait_for_state("the pane to be retained as exited", |s| {
                find(s, pane).is_some_and(is_exited)
            })
            .await;
        find(&snapshot, pane).cloned().expect("listed")
    }

    /// The next `RESOURCE_CLOSED` on the owner.
    async fn next_close(&mut self) -> Closed {
        loop {
            let (_, frame) = timeout(DEADLINE, recv_typed(&mut self.owner))
                .await
                .expect("a RESOURCE_CLOSED arrives");
            if let FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } = frame
            {
                return Closed {
                    terminal_id,
                    exit_status,
                    reason,
                    signal,
                };
            }
        }
    }

    /// No `RESOURCE_CLOSED` for `pane` reaches the owner for [`QUIET`].
    async fn assert_no_close(&mut self, pane: &ResourceId) {
        let until = Instant::now() + QUIET;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            let Ok((_, frame)) = timeout(left, recv_typed(&mut self.owner)).await else {
                return;
            };
            if let FrameKind::ResourceClosed { terminal_id, .. } = &frame {
                assert_ne!(terminal_id, pane, "a retained pane is not closed at exit");
            }
        }
    }

    /// The next event the server journals for `pane`.
    async fn next_event_for(&mut self, pane: &ResourceId) -> AgentEvent {
        loop {
            let (_, frame) = timeout(DEADLINE, recv_typed(&mut self.events))
                .await
                .expect("an event arrives");
            if let FrameKind::Event {
                terminal: Some(terminal),
                event,
                ..
            } = frame
                && &terminal == pane
            {
                return event;
            }
        }
    }

    /// Events for `pane` up to its `terminal_control { Exited }`, which is
    /// returned separately.
    async fn events_until_exited(&mut self, pane: &ResourceId) -> (AgentEvent, Vec<AgentEvent>) {
        let mut earlier = Vec::new();
        loop {
            let event = self.next_event_for(pane).await;
            if is_exited_control(&event) {
                return (event, earlier);
            }
            earlier.push(event);
        }
    }

    /// Events for `pane` up to (not including) its `pane_closed`.
    async fn events_until_closed(&mut self, pane: &ResourceId) -> Vec<AgentEvent> {
        let mut earlier = Vec::new();
        loop {
            let event = self.next_event_for(pane).await;
            if matches!(event, AgentEvent::ResourceClosed { .. }) {
                return earlier;
            }
            earlier.push(event);
        }
    }

    /// Create `name` as a keep-empty session seeded with a blocking pane,
    /// through `phux.session.create/v1`, and return the seed pane.
    async fn create_keep_empty(&mut self, name: &str) -> ResourceId {
        let request_id = self.request_id();
        let token = format!("00000000-0000-4000-8000-{request_id:012}");
        let body = serde_json::json!({
            "name": name,
            "command": ["/bin/sh", "-c", "read _"],
            "keep_empty": true,
            "request_token": token,
        });
        send_frame(
            &mut self.probe,
            &FrameKind::SetMetadata {
                request_id,
                scope: Scope::Global,
                key: SESSION_CREATE_KEY.to_owned(),
                value: serde_json::to_vec(&body).unwrap(),
            },
        )
        .await;
        let read_id = self.request_id();
        send_frame(
            &mut self.probe,
            &FrameKind::GetMetadata {
                request_id: read_id,
                scope: Scope::Global,
                key: format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{token}"),
            },
        )
        .await;
        loop {
            let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut self.probe))
                .await
                .expect("the create result is answered");
            if let FrameKind::MetadataValue { request_id, value } = frame
                && request_id == read_id
            {
                let result: serde_json::Value =
                    serde_json::from_slice(&value.expect("a create result")).unwrap();
                let id = result["terminal_id"].as_u64().expect("a seed pane");
                return ResourceId::local(u32::try_from(id).unwrap());
            }
        }
    }
}

fn key(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(str::to_owned),
        unshifted_codepoint: None,
    }
}

fn find<'a>(snapshot: &'a SessionSnapshot, pane: &ResourceId) -> Option<&'a ResourceInfo> {
    snapshot.resources.iter().find(|r| &r.id == pane)
}

fn session<'a>(snapshot: &'a SessionSnapshot, name: &str) -> Option<&'a SessionInfo> {
    snapshot.sessions.iter().find(|s| s.name == name)
}

const fn is_exited(info: &ResourceInfo) -> bool {
    matches!(info.lifecycle, ResourceLifecycle::Exited)
}

const fn is_exited_control(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TerminalControl {
            action: ControlAction::Exited,
            ..
        }
    )
}

fn assert_error(result: &CommandResult, want: ErrorCode, what: &str) {
    assert!(
        matches!(result, CommandResult::Error { code, .. } if *code == want),
        "{what}: expected {want:?}, got {result:?}"
    );
}

#[test]
fn retained_pane_reports_exit_facet_in_get_state_and_stays_readable() {
    run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h
            .spawn("read _; echo RETAIN_MARK; exit 7", Some(600), None)
            .await;
        h.release(&pane).await;

        let info = h.wait_exited(&pane).await;
        let exit = info.exit.expect("a retained pane carries its exit facet");
        assert_eq!(exit.exit_status, Some(7));
        assert_eq!(exit.signal, None);
        assert_eq!(exit.reason, CloseReason::Exited);
        assert_eq!(
            exit.retained_until_ms - exit.exited_at_ms,
            600_000,
            "retained for the requested 600 seconds"
        );

        let screen = h
            .command(Command::GetScreen {
                terminal_id: pane.clone(),
                request_scrollback: None,
                cells: false,
                format: 0,
            })
            .await;
        assert!(
            format!("{screen:?}").contains("RETAIN_MARK"),
            "GET_SCREEN reads the last grid: {screen:?}"
        );
        let history = h
            .command(Command::GetScreen {
                terminal_id: pane.clone(),
                request_scrollback: Some(0),
                cells: false,
                format: 0,
            })
            .await;
        assert!(
            format!("{history:?}").contains("RETAIN_MARK"),
            "history still answers: {history:?}"
        );

        let CommandResult::OkWith(CommandValue::Json(json)) = h
            .command(Command::GetTerminalState {
                terminal_id: pane.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            })
            .await
        else {
            panic!("GET_TERMINAL_STATE answers a retained pane");
        };
        let state: serde_json::Value = serde_json::from_str(&json).unwrap();
        let process_exit = &state["process"]["exit"];
        assert_eq!(process_exit["status"], 7, "{state}");
        assert_eq!(process_exit["reason"], "exited", "{state}");
        assert_eq!(
            process_exit["exited_at_ms"], exit.exited_at_ms,
            "GET_TERMINAL_STATE and GET_STATE report one exit record"
        );
        h.stop().await;
    });
}

#[test]
fn retained_pane_emits_terminal_control_exited_not_resource_closed() {
    run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h.spawn("read _; exit 7", Some(600), None).await;
        h.release(&pane).await;

        let (exited, earlier) = h.events_until_exited(&pane).await;
        let AgentEvent::TerminalControl {
            lifecycle,
            exit_status,
            actor,
            ..
        } = exited
        else {
            unreachable!("filtered on terminal_control");
        };
        assert!(matches!(lifecycle, ResourceLifecycle::Exited));
        assert_eq!(exit_status, Some(7));
        assert_eq!(actor, None, "the exit is server-driven");
        assert!(
            !earlier
                .iter()
                .any(|event| matches!(event, AgentEvent::ResourceClosed { .. })),
            "no pane_closed at exit: {earlier:?}"
        );
        h.assert_no_close(&pane).await;
        assert!(find(&h.state().await, &pane).is_some_and(is_exited));

        // The purge is the close, and it carries the exit it retained.
        assert!(matches!(
            h.command(Command::KillResource {
                terminal_id: pane.clone(),
                operation_id: None,
            })
            .await,
            CommandResult::Ok
        ));
        let closed = h.next_close().await;
        assert_eq!(
            closed,
            Closed {
                terminal_id: pane.clone(),
                exit_status: Some(7),
                reason: CloseReason::Killed,
                signal: None,
            }
        );
        let _ = h.events_until_closed(&pane).await;
        h.stop().await;
    });
}

#[test]
fn purge_after_ttl_emits_resource_closed_exited_and_cascades_children() {
    run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h.spawn("read _; exit 0", Some(1), None).await;
        let child = h.spawn_child(&pane).await;
        h.release(&pane).await;

        let _ = h.wait_exited(&pane).await;
        assert!(
            find(&h.state().await, &child).is_some(),
            "a child outlives its parent's process while the parent is retained"
        );

        let first = h.next_close().await;
        assert_eq!(first.terminal_id, child, "children close first");
        assert_eq!(first.reason, CloseReason::ParentClosed);
        let second = h.next_close().await;
        assert_eq!(
            second,
            Closed {
                terminal_id: pane.clone(),
                exit_status: Some(0),
                reason: CloseReason::Exited,
                signal: None,
            }
        );
        let snapshot = h.state().await;
        assert!(find(&snapshot, &pane).is_none() && find(&snapshot, &child).is_none());
        h.stop().await;
    });
}

#[test]
fn kill_of_a_retained_pane_purges_with_killed_and_is_idempotent() {
    run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h.spawn("read _; exit 0", Some(600), None).await;
        h.release(&pane).await;
        let _ = h.wait_exited(&pane).await;

        let kill = h
            .command(Command::KillResource {
                terminal_id: pane.clone(),
                operation_id: None,
            })
            .await;
        assert!(matches!(kill, CommandResult::Ok), "{kill:?}");
        let again = h
            .command(Command::KillResources {
                ids: vec![pane.clone()],
                operation_id: None,
            })
            .await;
        assert!(
            matches!(again, CommandResult::Ok),
            "a repeated kill is not an error: {again:?}"
        );

        let closed = h.next_close().await;
        assert_eq!(closed.terminal_id, pane);
        assert_eq!(closed.reason, CloseReason::Killed);
        assert_eq!(closed.exit_status, Some(0));
        h.assert_no_close(&pane).await;
        assert!(find(&h.state().await, &pane).is_none());
        h.stop().await;
    });
}

#[test]
fn retained_count_bound_purges_oldest_first() {
    run_local(async {
        let mut h = Harness::start(|cfg| {
            cfg.retain = RetainPolicy {
                max_count: 1,
                ..RetainPolicy::default()
            };
        })
        .await;
        let first = h.spawn("read _; exit 1", Some(600), None).await;
        h.release(&first).await;
        let _ = h.wait_exited(&first).await;

        let second = h.spawn("read _; exit 2", Some(600), None).await;
        h.release(&second).await;
        let closed = h.next_close().await;
        assert_eq!(
            closed,
            Closed {
                terminal_id: first.clone(),
                exit_status: Some(1),
                reason: CloseReason::Exited,
                signal: None,
            },
            "retaining a second pane evicts the first"
        );
        let info = h.wait_exited(&second).await;
        assert_eq!(info.exit.and_then(|exit| exit.exit_status), Some(2));
        assert!(find(&h.state().await, &first).is_none());
        h.stop().await;
    });
}

#[test]
fn input_to_a_retained_pane_is_input_not_written_and_signal_is_invalid_command() {
    run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h.spawn("read _; exit 0", Some(600), None).await;
        h.release(&pane).await;
        let info = h.wait_exited(&pane).await;

        let routed = h
            .command(Command::RouteInput {
                terminal_id: pane.clone(),
                event: InputEvent::Key(key(PhysicalKey::A, Some("a"))),
            })
            .await;
        assert_error(&routed, ErrorCode::InputNotWritten, "ROUTE_INPUT");
        let applied = h
            .command(Command::ApplyInput {
                operation_id: InputOperationId::new([7; 16]).unwrap(),
                terminal_id: pane.clone(),
                events: vec![InputEvent::Key(key(PhysicalKey::A, Some("a")))],
            })
            .await;
        assert_error(&applied, ErrorCode::InputNotWritten, "APPLY_INPUT");
        let signalled = h
            .command(Command::SignalTerminal {
                terminal_id: pane.clone(),
                signal: TerminalSignal::Interrupt,
                operation_id: None,
            })
            .await;
        assert_error(&signalled, ErrorCode::InvalidCommand, "SIGNAL_TERMINAL");

        send_frame(
            &mut h.probe,
            &FrameKind::ResizeTerminal {
                terminal_id: pane.clone(),
                cols: 120,
                rows: 40,
            },
        )
        .await;
        let after = find(&h.state().await, &pane).cloned().expect("listed");
        assert_eq!(
            (after.cols, after.rows),
            (info.cols, info.rows),
            "RESIZE_TERMINAL is a no-op on a retained pane"
        );
        assert!(is_exited(&after));
        h.stop().await;
    });
}

#[test]
fn unretained_spawn_is_byte_identical_and_reaps_as_before() {
    run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h.spawn("read _; exit 7", None, None).await;
        h.release(&pane).await;

        let closed = h.next_close().await;
        assert_eq!(
            closed,
            Closed {
                terminal_id: pane.clone(),
                exit_status: Some(7),
                reason: CloseReason::Exited,
                signal: None,
            }
        );
        let events = h.events_until_closed(&pane).await;
        assert!(
            !events.iter().any(is_exited_control),
            "an unretained exit sends no terminal_control {{ exited }}: {events:?}"
        );
        let snapshot = h.state().await;
        assert!(find(&snapshot, &pane).is_none(), "reaped at exit");
        assert!(
            snapshot
                .resources
                .iter()
                .all(|r| r.exit.is_none() && matches!(r.lifecycle, ResourceLifecycle::Running)),
            "no resource state, so the snapshot carries no extension block"
        );
        h.stop().await;
    });
}

#[test]
fn keep_empty_session_and_retained_pane_compose() {
    run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let seed = h.create_keep_empty("kept").await;
        let pane = h.spawn("read _; exit 4", Some(600), Some(&seed)).await;
        h.release(&pane).await;
        let _ = h.wait_exited(&pane).await;

        let kill = h
            .command(Command::KillResource {
                terminal_id: seed.clone(),
                operation_id: None,
            })
            .await;
        assert!(matches!(kill, CommandResult::Ok), "{kill:?}");
        let snapshot = h
            .wait_for_state("the seed pane to be reaped", |s| find(s, &seed).is_none())
            .await;
        let kept = session(&snapshot, "kept").expect("the session is still there");
        assert!(
            !kept.is_empty(),
            "the retained pane still occupies its window"
        );
        assert!(find(&snapshot, &pane).is_some_and(is_exited));

        let kill = h
            .command(Command::KillResource {
                terminal_id: pane.clone(),
                operation_id: None,
            })
            .await;
        assert!(matches!(kill, CommandResult::Ok), "{kill:?}");
        let snapshot = h
            .wait_for_state("the retained pane to be purged", |s| {
                find(s, &pane).is_none()
            })
            .await;
        let kept = session(&snapshot, "kept").expect("a keep-empty session outlives its purge");
        assert!(kept.is_empty(), "its window went with its last pane");
        h.stop().await;
    });
}

/// `defaults.retain-on-exit = true` means every pane, the session's seed
/// pane included, not only panes spawned by `SPAWN_RESOURCE`.
#[test]
fn a_seed_pane_is_retained_when_the_operator_makes_retention_the_default() {
    run_local(async {
        let mut h = Harness::start(|cfg| {
            let mut seed = portable_pty::CommandBuilder::new("/bin/sh");
            seed.args(["-c", "read _; exit 3"]);
            cfg.seed_with_pty = true;
            cfg.seed_command = Some(seed);
            cfg.retain = RetainPolicy {
                by_default: true,
                ..RetainPolicy::default()
            };
        })
        .await;
        let seed = h
            .state()
            .await
            .resources
            .first()
            .map(|r| r.id.clone())
            .expect("the seed pane");
        // A last-shell natural exit is replaced in place (ADR-0131). Keep a
        // sibling live so this seed can actually be retained as exited.
        let _sibling = h.spawn("read _", None, Some(&seed)).await;
        h.release(&seed).await;
        let info = h.wait_exited(&seed).await;
        assert_eq!(
            info.exit.and_then(|exit| exit.exit_status),
            Some(3),
            "the seed pane is retained with its status"
        );
        h.stop().await;
    });
}
