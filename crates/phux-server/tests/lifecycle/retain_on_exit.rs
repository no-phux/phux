//! ADR-0124 retain on exit: a retained Terminal's exit is a facet, and its
//! close is a purge. `owner` spawns (and so receives `RESOURCE_CLOSED`),
//! `probe` carries commands, and `events` the journal, so a reply never
//! races a close on one socket. Panes block on `read` until released.

use std::time::Duration;

use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, CommandValue, ControlAction, ErrorCode,
    FrameKind, ResourceLifecycle, SpawnResource, TerminalSignal,
};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
use phux_server::ServerConfig;
use phux_server::state::RetainPolicy;
use phux_server_testkit::{Spawn, ascii_key, command, recv_typed, send_frame};
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::{
    Seen, Server, attach, create_seeded, find, next_event, release, session, sh, spawned, state,
    subscribe, wait_for_state, wait_frame,
};

/// How long the owner must stay free of a `RESOURCE_CLOSED` that must not come.
const QUIET: Duration = Duration::from_millis(300);

/// One `RESOURCE_CLOSED`: (id, exit status, reason, signal).
type Closed = (ResourceId, Option<i32>, CloseReason, Option<i32>);

struct Harness {
    server: Server,
    owner: UnixStream,
    probe: UnixStream,
    events: UnixStream,
    next_request: u32,
}

impl Harness {
    async fn start(configure: impl FnOnce(&mut ServerConfig)) -> Self {
        let server = Server::start(Some("demo"), configure);
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let probe = server.connect().await;
        let mut events = server.connect().await;
        subscribe(&mut events, 1, None, None).await;
        Self {
            server,
            owner,
            probe,
            events,
            next_request: 100,
        }
    }

    async fn stop(self) {
        drop((self.owner, self.probe, self.events));
        self.server.stop().await;
    }

    const fn request_id(&mut self) -> u32 {
        self.next_request += 1;
        self.next_request
    }

    /// Spawn `sh -c script` from the owner, retained for `retain` seconds.
    async fn spawn(
        &mut self,
        script: &str,
        retain: Option<u32>,
        beside: Option<&ResourceId>,
    ) -> ResourceId {
        let spawn = Spawn {
            owner_terminal: beside.cloned(),
            resource: retain
                .map(|secs| Box::new(SpawnResource::default().with_retain_secs(Some(secs)))),
            ..sh(script)
        };
        let request_id = self.request_id();
        spawned(&mut self.owner, request_id, spawn).await
    }

    async fn command(&mut self, cmd: Command) -> CommandResult {
        let request_id = self.request_id();
        command(&mut self.probe, request_id, cmd).await
    }

    async fn state(&mut self) -> SessionSnapshot {
        let request_id = self.request_id();
        state(&mut self.probe, request_id).await
    }

    async fn wait_exited(&mut self, pane: &ResourceId) -> ResourceInfo {
        let request_id = self.request_id() * 100;
        let snapshot = wait_for_state(
            &mut self.probe,
            request_id,
            "the pane to be retained",
            |s| find(s, pane).is_some_and(is_exited),
        )
        .await;
        find(&snapshot, pane).cloned().unwrap()
    }

    async fn kill(&mut self, pane: &ResourceId) {
        let kill = Command::KillResource {
            terminal_id: pane.clone(),
            operation_id: None,
        };
        assert_eq!(self.command(kill).await, CommandResult::Ok);
    }

    async fn next_close(&mut self) -> Closed {
        wait_frame(&mut self.owner, "RESOURCE_CLOSED", |frame| match frame {
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => Some((terminal_id, exit_status, reason, signal)),
            _ => None,
        })
        .await
    }

    /// No `RESOURCE_CLOSED` for `pane` reaches the owner for [`QUIET`].
    async fn assert_no_close(&mut self, pane: &ResourceId) {
        let _ = timeout(QUIET, async {
            loop {
                if let (_, FrameKind::ResourceClosed { terminal_id, .. }) =
                    recv_typed(&mut self.owner).await
                {
                    assert_ne!(&terminal_id, pane, "a retained pane is not closed at exit");
                }
            }
        })
        .await;
    }

    /// Events for `pane` up to and including the first `last` accepts.
    async fn events_until(
        &mut self,
        pane: &ResourceId,
        last: impl Fn(&AgentEvent) -> bool,
    ) -> Vec<AgentEvent> {
        let mut seen = Vec::new();
        loop {
            let event = next_event(&mut self.events, |e: &Seen| {
                e.terminal.as_ref() == Some(pane)
            })
            .await
            .event;
            let done = last(&event);
            seen.push(event);
            if done {
                return seen;
            }
        }
    }
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

const fn is_closed(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::ResourceClosed { .. })
}

fn assert_error(result: &CommandResult, want: ErrorCode, what: &str) {
    assert!(
        matches!(result, CommandResult::Error { code, .. } if *code == want),
        "{what}: {result:?}"
    );
}

/// A retained pane refuses input and signals; resize is a no-op.
async fn assert_inert(h: &mut Harness, pane: &ResourceId, info: &ResourceInfo) {
    let key = || InputEvent::Key(ascii_key('a', PhysicalKey::A));
    let routed = h
        .command(Command::RouteInput {
            terminal_id: pane.clone(),
            event: key(),
        })
        .await;
    assert_error(&routed, ErrorCode::InputNotWritten, "ROUTE_INPUT");
    let applied = h
        .command(Command::ApplyInput {
            operation_id: InputOperationId::new([7; 16]).unwrap(),
            terminal_id: pane.clone(),
            events: vec![key()],
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
            cell_px: None,
        },
    )
    .await;
    let after = find(&h.state().await, pane).cloned().unwrap();
    assert_eq!(
        (after.cols, after.rows),
        (info.cols, info.rows),
        "resize is a no-op"
    );
    assert!(is_exited(&after));
}

/// A retained exit journals `terminal_control { exited }` instead of
/// `pane_closed`, lists the exit facet, keeps the last grid and history
/// readable, refuses input and signals (resize is a no-op), and its kill is
/// an idempotent purge carrying the retained exit.
#[test]
fn a_retained_exit_is_a_readable_facet_until_its_purge() {
    phux_server_testkit::run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h
            .spawn("read _; echo RETAIN_MARK; exit 7", Some(600), None)
            .await;
        release(&mut h.owner, &pane).await;

        let events = h.events_until(&pane, is_exited_control).await;
        assert!(
            !events.iter().any(is_closed),
            "no pane_closed at exit: {events:?}"
        );
        let Some(AgentEvent::TerminalControl {
            lifecycle,
            exit_status,
            actor,
            ..
        }) = events.last()
        else {
            unreachable!()
        };
        assert!(matches!(lifecycle, ResourceLifecycle::Exited));
        assert_eq!(
            (*exit_status, *actor),
            (Some(7), None),
            "server-driven exit"
        );
        h.assert_no_close(&pane).await;

        let info = h.wait_exited(&pane).await;
        let exit = info.exit.expect("the exit facet");
        assert_eq!(
            (exit.exit_status, exit.signal, exit.reason),
            (Some(7), None, CloseReason::Exited)
        );
        assert_eq!(exit.retained_until_ms - exit.exited_at_ms, 600_000);
        for scrollback in [None, Some(0)] {
            let screen = h
                .command(Command::GetScreen {
                    terminal_id: pane.clone(),
                    request_scrollback: scrollback,
                    cells: false,
                    format: 0,
                })
                .await;
            assert!(
                format!("{screen:?}").contains("RETAIN_MARK"),
                "{scrollback:?}: {screen:?}"
            );
        }
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
        let terminal_state: serde_json::Value = serde_json::from_str(&json).unwrap();
        let process_exit = &terminal_state["process"]["exit"];
        assert_eq!(process_exit["status"], 7);
        assert_eq!(process_exit["reason"], "exited");
        assert_eq!(
            process_exit["exited_at_ms"], exit.exited_at_ms,
            "one exit record"
        );

        assert_inert(&mut h, &pane, &info).await;

        h.kill(&pane).await;
        let again = h
            .command(Command::KillResources {
                ids: vec![pane.clone()],
                operation_id: None,
            })
            .await;
        assert_eq!(again, CommandResult::Ok, "a repeated kill is not an error");
        assert_eq!(
            h.next_close().await,
            (pane.clone(), Some(7), CloseReason::Killed, None)
        );
        h.events_until(&pane, is_closed).await;
        h.assert_no_close(&pane).await;
        assert!(find(&h.state().await, &pane).is_none());
        h.stop().await;
    });
}

/// At the TTL the purge closes children first (`ParentClosed`), then the
/// pane with its retained exit (`Exited`).
#[test]
fn purge_after_ttl_closes_the_pane_exited_and_cascades_children() {
    phux_server_testkit::run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h.spawn("read _; exit 0", Some(1), None).await;
        let child = Spawn {
            resource: Some(Box::new(SpawnResource::agent_session(
                pane.clone(),
                "claude",
            ))),
            ..Spawn::default()
        };
        let request_id = h.request_id();
        let child = spawned(&mut h.owner, request_id, child).await;
        release(&mut h.owner, &pane).await;
        h.wait_exited(&pane).await;
        assert!(
            find(&h.state().await, &child).is_some(),
            "the child outlives the process"
        );

        let first = h.next_close().await;
        assert_eq!(
            (&first.0, first.2),
            (&child, CloseReason::ParentClosed),
            "children close first"
        );
        assert_eq!(
            h.next_close().await,
            (pane.clone(), Some(0), CloseReason::Exited, None)
        );
        let snapshot = h.state().await;
        assert!(find(&snapshot, &pane).is_none() && find(&snapshot, &child).is_none());
        h.stop().await;
    });
}

#[test]
fn retained_count_bound_purges_oldest_first() {
    phux_server_testkit::run_local(async {
        let mut h = Harness::start(|cfg| {
            cfg.retain = RetainPolicy {
                max_count: 1,
                ..RetainPolicy::default()
            };
        })
        .await;
        let first = h.spawn("read _; exit 1", Some(600), None).await;
        release(&mut h.owner, &first).await;
        h.wait_exited(&first).await;
        let second = h.spawn("read _; exit 2", Some(600), None).await;
        release(&mut h.owner, &second).await;
        assert_eq!(
            h.next_close().await,
            (first.clone(), Some(1), CloseReason::Exited, None)
        );
        let info = h.wait_exited(&second).await;
        assert_eq!(info.exit.and_then(|exit| exit.exit_status), Some(2));
        assert!(find(&h.state().await, &first).is_none());
        h.stop().await;
    });
}

#[test]
fn unretained_exit_closes_and_reaps_as_before() {
    phux_server_testkit::run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let pane = h.spawn("read _; exit 7", None, None).await;
        release(&mut h.owner, &pane).await;
        assert_eq!(
            h.next_close().await,
            (pane.clone(), Some(7), CloseReason::Exited, None)
        );
        let events = h.events_until(&pane, is_closed).await;
        assert!(!events.iter().any(is_exited_control), "{events:?}");
        let snapshot = h.state().await;
        assert!(find(&snapshot, &pane).is_none());
        assert!(
            snapshot
                .resources
                .iter()
                .all(|r| r.exit.is_none() && matches!(r.lifecycle, ResourceLifecycle::Running)),
            "no resource state, so no extension block"
        );
        h.stop().await;
    });
}

/// A retained pane keeps a keep-empty session's window after the seed is
/// killed; purging it empties the window and the session survives.
#[test]
fn keep_empty_session_and_retained_pane_compose() {
    phux_server_testkit::run_local(async {
        let mut h = Harness::start(|_| {}).await;
        let request_id = h.request_id();
        let seed = create_seeded(&mut h.probe, request_id, "kept", true).await;
        let pane = h.spawn("read _; exit 4", Some(600), Some(&seed)).await;
        release(&mut h.owner, &pane).await;
        h.wait_exited(&pane).await;

        h.kill(&seed).await;
        let snapshot = wait_for_state(&mut h.probe, 5000, "the seed to be reaped", |s| {
            find(s, &seed).is_none()
        })
        .await;
        assert!(
            !session(&snapshot, "kept").unwrap().is_empty(),
            "the retained pane holds the window"
        );
        assert!(find(&snapshot, &pane).is_some_and(is_exited));

        h.kill(&pane).await;
        let snapshot = wait_for_state(&mut h.probe, 6000, "the purge", |s| {
            find(s, &pane).is_none()
        })
        .await;
        assert!(
            session(&snapshot, "kept").unwrap().is_empty(),
            "the window went with its last pane"
        );
        h.stop().await;
    });
}

/// `retain-on-exit = true` retains every pane, the seed pane included. A
/// sibling stays live because a last-shell exit is replaced in place.
#[test]
fn a_seed_pane_is_retained_when_retention_is_the_default() {
    phux_server_testkit::run_local(async {
        let mut h = Harness::start(|cfg| {
            let mut seed = portable_pty::CommandBuilder::new("/bin/sh");
            seed.args(["-c", "read _; exit 3"]);
            phux_server_testkit::seed_pty(cfg, seed);
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
            .unwrap();
        let _sibling = h.spawn("read _", None, Some(&seed)).await;
        release(&mut h.owner, &seed).await;
        assert_eq!(
            h.wait_exited(&seed)
                .await
                .exit
                .and_then(|exit| exit.exit_status),
            Some(3)
        );
        h.stop().await;
    });
}
