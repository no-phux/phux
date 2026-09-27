//! `AgentEvent::Asked` (SPEC §7.5): a pending human-answerable question
//! reaches `EVENT` subscribers from either a `phux-ask` title sentinel or a
//! `REPORT_ASKED` hook, and one question from both sources is told once
//! (ADR-0036 ladder).

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, ErrorCode, FrameKind};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    WIRE_RECV_TIMEOUT, join_after_shutdown, recv_until, recv_until_deadline, run_local, send_frame,
    spawn_server_with_seed_cmd,
};

use crate::common::{attach_pane, connect_as, full_caps, gated_seed, subscribe_all};

fn report_asked(terminal: ResourceId, id: &str, question: &str, suggestions: &[&str]) -> Command {
    Command::ReportAsked {
        terminal_id: terminal,
        id: id.to_owned(),
        question: question.to_owned(),
        suggestions: suggestions.iter().map(|s| (*s).to_owned()).collect(),
        elapsed_seconds: None,
    }
}

/// Send `command` and return its reply plus every `Asked` that preceded it.
/// The server queues an `Asked` before answering, so "none before the reply"
/// is an exact negative.
async fn asked_before_reply(
    stream: &mut UnixStream,
    request_id: u32,
    command: Command,
) -> (CommandResult, Vec<AgentEvent>) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    let mut asked = Vec::new();
    let result = recv_until(stream, |_, frame| match frame {
        FrameKind::CommandResult {
            request_id: got,
            result,
        } if got == request_id => Some(result),
        FrameKind::Event {
            event: event @ AgentEvent::Asked { .. },
            ..
        } => {
            asked.push(event);
            None
        }
        _ => None,
    })
    .await;
    (result, asked)
}

/// The title sentinel `phux-ask[q1]:Deploy to prod??s=Yes|No|Hold` parses into
/// an `Asked`; the hook then reporting the same question emits nothing new.
#[test]
fn ask_title_sentinel_emits_asked_and_a_repeating_hook_does_not_re_emit() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let seed = gated_seed(
            &release,
            "printf '\\033]2;phux-ask[q1]:Deploy to prod??s=Yes|No|Hold\\007'; sleep 60",
        );
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", seed);
        let (mut stream, _) = connect_as(&socket_path, "agent-asked", full_caps()).await;
        let pane = attach_pane(&mut stream, "demo").await;
        subscribe_all(&mut stream, 1).await;
        std::fs::write(&release, b"go").unwrap();

        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let asked = recv_until_deadline(&mut stream, deadline, |_, frame| match frame {
            FrameKind::Event {
                event: event @ AgentEvent::Asked { .. },
                ..
            } => Some(event),
            _ => None,
        })
        .await;
        assert_eq!(
            asked,
            Some(AgentEvent::Asked {
                id: "q1".to_owned(),
                question: "Deploy to prod?".to_owned(),
                suggestions: vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
                elapsed_seconds: None,
            })
        );

        let hook = report_asked(pane, "q1", "Deploy to prod?", &["Yes", "No", "Hold"]);
        let (result, duplicates) = asked_before_reply(&mut stream, 9, hook).await;
        assert_eq!(result, CommandResult::Ok, "the hook still owns the ask");
        assert!(
            duplicates.is_empty(),
            "one question is told once: {duplicates:?}"
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

#[test]
fn report_asked_emits_asked_and_rejects_an_empty_question() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let mut park = CommandBuilder::new("/bin/sh");
        park.args(["-c", "sleep 600"]);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", park);
        let (mut stream, _) = connect_as(&socket_path, "agent-asked", full_caps()).await;
        let pane = attach_pane(&mut stream, "demo").await;
        subscribe_all(&mut stream, 1).await;

        let mut hook = report_asked(
            pane.clone(),
            "hook-q1",
            "Approve release?",
            &["Ship", "Hold"],
        );
        if let Command::ReportAsked {
            elapsed_seconds, ..
        } = &mut hook
        {
            *elapsed_seconds = Some(12);
        }
        let (result, asked) = asked_before_reply(&mut stream, 7, hook).await;
        assert_eq!(result, CommandResult::Ok);
        assert_eq!(
            asked,
            [AgentEvent::Asked {
                id: "hook-q1".to_owned(),
                question: "Approve release?".to_owned(),
                suggestions: vec!["Ship".to_owned(), "Hold".to_owned()],
                elapsed_seconds: Some(12),
            }]
        );

        let (result, _) =
            asked_before_reply(&mut stream, 8, report_asked(pane, "bad", "   ", &[])).await;
        let CommandResult::Error { code, message } = result else {
            panic!("an empty question must be rejected: {result:?}");
        };
        assert_eq!(code, ErrorCode::InvalidCommand);
        assert!(message.contains("question"), "{message}");

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
