//! Drive the public argv through a private, protocol-scripted Unix socket.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "CLI test assertions"
)]

#[path = "common/ambient.rs"]
mod ambient;
#[path = "common/runner.rs"]
mod runner;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use phux_client::testkit::{EndOfScript, ScriptSpec, ScriptedServer};
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{AgentEvent, EventStamp, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio::process::Command;

fn event(event: AgentEvent, seq: u64) -> FrameKind {
    pane_event(7, event, seq)
}

fn pane_event(terminal: u32, event: AgentEvent, seq: u64) -> FrameKind {
    FrameKind::Event {
        terminal: Some(ResourceId::local(terminal)),
        event,
        stamp: Some(Box::new(EventStamp::new(seq, 1000 + seq))),
    }
}

fn server(
    path: &Path,
    events: Vec<FrameKind>,
    end: EndOfScript,
) -> tokio::task::JoinHandle<Vec<FrameKind>> {
    let listener = UnixListener::bind(path).unwrap();
    let id = ResourceId::local(7);
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    let state = SessionSnapshot::new(session, window, id.clone())
        .with_resources(vec![
            ResourceInfo::new(id, window, 80, 24).with_session(Some(session)),
        ])
        .with_journal_head(Some(40));
    tokio::spawn(async move {
        // Target resolution uses a separate GET_STATE connection, exactly as watch.
        ScriptedServer::accept(&listener, ScriptSpec::new().state(state.clone())).await;
        ScriptedServer::accept(
            &listener,
            ScriptSpec::new()
                .server_id(vec![1])
                .state(state)
                .extend(events)
                .end(end),
        )
        .await
    })
}

fn cli(socket: &Path, args: &[&str]) -> Command {
    let mut cmd = ambient::phux_cmd(runner::phux_bin());
    cmd.arg("--socket").arg(socket).args(args);
    let mut cmd = Command::from(cmd);
    cmd.kill_on_drop(true);
    cmd
}

#[tokio::test]
async fn dispatches_only_exact_live_matches_with_watch_json_and_literal_argv() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("sock");
    let end = AgentEvent::ResourceClosed {
        exit_status: Some(0),
    };
    let server = server(
        &socket,
        vec![
            event(AgentEvent::Dirty, 41),
            pane_event(8, end.clone(), 42),
            event(AgentEvent::Bell, 43),
            event(AgentEvent::Bell, 44),
            event(end, 45),
            event(AgentEvent::Bell, 46),
        ],
        EndOfScript::ServeUntilDetach,
    );
    let code = r#"
        test -z "${PHUX_WS_TOKENS+x}${OPENAI_API_KEY+x}${PHUX_SOCKET+x}" || exit 42
        printf '%s\n' "$@"
        /bin/cat
    "#;
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        cli(
            &socket,
            &[
                "on",
                "bell",
                "--target",
                "@7",
                "--",
                "/bin/sh",
                "-c",
                code,
                "_",
                "--json",
                "--capabilities",
                "$(not shell syntax)",
                "--socket",
            ],
        )
        .env("PHUX_WS_TOKENS", "dummy-token")
        .env("PHUX_SOCKET", &socket)
        .env("OPENAI_API_KEY", "dummy-credential")
        .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let out = String::from_utf8(output.stdout).unwrap();
    let mut lines = out.lines();
    for seq in [43, 44] {
        assert_eq!(lines.next(), Some("--json"));
        assert_eq!(lines.next(), Some("--capabilities"));
        assert_eq!(lines.next(), Some("$(not shell syntax)"));
        assert_eq!(lines.next(), Some("--socket"));
        let line: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(
            line,
            json!({"event":"bell","terminal":"@7","seq":seq,"ts_ms":1000+seq})
        );
    }
    assert_eq!(lines.next(), None);
    let seen = server.await.unwrap();
    assert!(seen.iter().any(|frame| matches!(
        frame,
        FrameKind::SubscribeEvents {
            after_seq: Some(40),
            ..
        }
    )));
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SubscribeMetadata { .. }))
    );
}

#[tokio::test]
async fn a_gap_stops_dispatch_and_names_the_missing_data() {
    let cases = [
        (
            "journal_gap",
            AgentEvent::JournalGap {
                first_missing: 41,
                last_missing: 43,
            },
        ),
        ("source_gap", AgentEvent::SourceGap { dropped: 3 }),
    ];
    for (name, gap) in cases {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sock");
        let server = server(
            &socket,
            vec![event(gap, 44), event(AgentEvent::Bell, 45)],
            EndOfScript::ServeUntilDetach,
        );
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            cli(&socket, &["on", "bell", "--target", "@7", "--", "/bin/cat"]).output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(name),
            "{out:?}"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn handler_failure_stops_dispatch_without_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("sock");
    let server = server(
        &socket,
        vec![event(AgentEvent::Bell, 41), event(AgentEvent::Bell, 42)],
        EndOfScript::ServeUntilDetach,
    );
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        cli(
            &socket,
            &["on", "bell", "--", "/bin/sh", "-c", "echo hit; exit 42"],
        )
        .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(out.stdout, b"hit\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("42"),
        "{out:?}"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn disconnect_is_nonzero_and_does_not_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("sock");
    let server = server(&socket, vec![], EndOfScript::HangUp);
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        cli(&socket, &["on", "bell", "--", "/bin/cat"]).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no reconnect"),
        "{out:?}"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn invalid_event_missing_separator_and_replay_are_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("missing.sock");
    for args in [
        &["on", "not-an-event", "--", "/bin/cat"][..],
        &["on", "journal_gap", "--", "/bin/cat"],
        &["on", "agent_state", "--", "/bin/cat"],
        &["on", "bell", "/bin/cat"],
        &["on", "bell", "--"],
        &["on", "bell", "--after", "01:0", "--", "/bin/cat"],
    ] {
        let out = tokio::time::timeout(Duration::from_secs(10), cli(&socket, args).output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{out:?}");
    }
}

#[tokio::test]
async fn sigterm_cancels_the_running_handler_group() {
    use rustix::process::{Pid, Signal, kill_process, test_kill_process_group};
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("sock");
    let marker = dir.path().join("handler");
    let survived = dir.path().join("survived");
    let server = server(
        &socket,
        vec![event(AgentEvent::Bell, 41)],
        EndOfScript::ServeUntilDetach,
    );
    let code = r#"(/bin/sleep 2; : >"$2") & printf '%s\n' "$$" >"$1"; wait"#;
    let mut child = cli(
        &socket,
        &[
            "on",
            "bell",
            "--target",
            "@7",
            "--",
            "/bin/sh",
            "-c",
            code,
            "_",
            marker.to_str().unwrap(),
            survived.to_str().unwrap(),
        ],
    )
    .stdout(Stdio::null())
    .spawn()
    .unwrap();
    let pid = |raw| Pid::from_raw(raw).unwrap();
    let handler = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(raw) = std::fs::read_to_string(&marker)
                && let Ok(raw) = raw.trim().parse::<i32>()
            {
                break pid(raw);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(test_kill_process_group(handler).is_ok());
    kill_process(
        pid(i32::try_from(child.id().unwrap()).unwrap()),
        Signal::TERM,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    // A container init may not reap orphaned zombies. Instead check that
    // the grandchild cannot do work after the subscription has exited.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!survived.exists());
    server.await.unwrap();
}
