//! The pushed agent-event stream (SPEC §7.5): subscribers receive `EVENT`s as
//! a pane retitles, bells, runs an OSC-133-marked command, changes directory,
//! and closes. Seeds are file-gated (see `common::gated_seed`) so no amount of
//! server-startup latency can let a pane speak before its subscriber listens.

use std::time::Duration;

use phux_protocol::wire::frame::{AgentEvent, FrameKind};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    Spawn, WIRE_RECV_TIMEOUT, join_after_shutdown, recv_typed_before, run_local, spawn_resource,
    spawn_server_with_seed_cmd,
};

use crate::common::{attach_pane, connect_as, full_caps, gated_seed, subscribe_all};

/// Drain `EVENT`s until `complete(&seen)` holds or `deadline` elapses.
async fn collect_events(
    stream: &mut UnixStream,
    deadline: Duration,
    complete: impl Fn(&[AgentEvent]) -> bool,
) -> Vec<AgentEvent> {
    let end = tokio::time::Instant::now() + deadline;
    let mut seen = Vec::new();
    while !complete(&seen) {
        let Some((_, frame)) = recv_typed_before(stream, end).await else {
            break;
        };
        if let FrameKind::Event { event, .. } = frame {
            seen.push(event);
        }
    }
    seen
}

fn has(seen: &[AgentEvent], pred: impl Fn(&AgentEvent) -> bool) -> bool {
    seen.iter().any(pred)
}

/// Title, bell, and `pane_closed { exit_status: Some(0) }` reach an attached
/// subscriber; `pane_closed` also reaches an unattached one (the `phux watch`
/// path, whose mailbox comes from the subscription registry).
#[test]
fn attached_and_unattached_subscribers_receive_title_bell_and_pane_closed() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let seed = gated_seed(
            &release,
            "printf '\\033]2;phux-watch\\007'; printf '\\007'; exit 0",
        );
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", seed);

        let (mut attached, _) = connect_as(&socket_path, "attached", full_caps()).await;
        attach_pane(&mut attached, "demo").await;
        // A live sibling, so the seed's exit closes it instead of replacing
        // the session's last shell in place (ADR-0131).
        let sibling = spawn_resource(
            &mut attached,
            2,
            Spawn::command(&["/bin/sh", "-c", "sleep 600"]),
        )
        .await;
        assert!(
            matches!(sibling, phux_protocol::wire::frame::SpawnResult::Ok(_)),
            "{sibling:?}"
        );
        subscribe_all(&mut attached, 1).await;
        let (mut watcher, _) = connect_as(&socket_path, "watcher", full_caps()).await;
        subscribe_all(&mut watcher, 1).await;
        std::fs::write(&release, b"go").unwrap();

        let closed = |e: &AgentEvent| matches!(e, AgentEvent::ResourceClosed { exit_status } if *exit_status == Some(0));
        let title = |e: &AgentEvent| matches!(e, AgentEvent::TitleChanged { title } if title == "phux-watch");
        let bell = |e: &AgentEvent| matches!(e, AgentEvent::Bell);
        let events = collect_events(&mut attached, WIRE_RECV_TIMEOUT, |seen| {
            has(seen, title) && has(seen, bell) && has(seen, closed)
        })
        .await;
        assert!(
            has(&events, title) && has(&events, bell) && has(&events, closed),
            "{events:?}"
        );
        let watched =
            collect_events(&mut watcher, WIRE_RECV_TIMEOUT, |seen| has(seen, closed)).await;
        assert!(has(&watched, closed), "unattached subscriber: {watched:?}");

        drop((attached, watcher));
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

/// OSC-133 `C`/`D` marks yield `command_started` and `command_finished` with
/// the `D` exit code (only the raw PTY scan can see it), and the prompt
/// boundary re-queries the kernel cwd.
#[test]
fn subscribed_client_receives_command_and_cwd_events() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let seed = gated_seed(
            &release,
            "cd /; printf '\\033]133;C\\007out\\r\\n\\033]133;D;7\\007'; sleep 60",
        );
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", seed);
        let (mut stream, _) = connect_as(&socket_path, "agent-events", full_caps()).await;
        attach_pane(&mut stream, "demo").await;
        subscribe_all(&mut stream, 1).await;
        std::fs::write(&release, b"go").unwrap();

        let started = |e: &AgentEvent| matches!(e, AgentEvent::CommandStarted);
        let finished = |e: &AgentEvent| matches!(e, AgentEvent::CommandFinished { exit_code } if *exit_code == Some(7));
        let cwd = |e: &AgentEvent| matches!(e, AgentEvent::CwdChanged { cwd } if cwd == "/");
        let events = collect_events(&mut stream, WIRE_RECV_TIMEOUT, |seen| {
            has(seen, started) && has(seen, finished) && has(seen, cwd)
        })
        .await;
        assert!(has(&events, started), "{events:?}");
        assert!(has(&events, finished), "{events:?}");
        assert!(has(&events, cwd), "{events:?}");

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
