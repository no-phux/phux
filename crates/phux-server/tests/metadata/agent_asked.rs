//! `AgentEvent::Asked` (SPEC §7.5): a pending human-answerable question
//! reaches `EVENT` subscribers from either a `phux-ask` title sentinel or a
//! `REPORT_ASKED` hook, and one question from both sources is told once
//! (ADR-0036 ladder).

use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::questions::{
    CurrentQuestion, CurrentQuestions, PtyQuestionSource, QuestionTarget,
};
use phux_protocol::native_agent::{NativeRequest, NativeRequestPayload};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SpawnedServer, WIRE_RECV_TIMEOUT, recv_until, recv_until_deadline, run_local, sh,
};

use crate::common::{attach_pane, connect_as, full_caps, gated_server, subscribe_all};

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
    phux_server_testkit::send_command(stream, request_id, command).await;
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

async fn current_questions(stream: &mut UnixStream, target: QuestionTarget) -> CurrentQuestions {
    let request = NativeRequestPayload::new(NativeRequest::CurrentQuestions { target }).unwrap();
    let (reply, _) = asked_before_reply(stream, 41, Command::NativeAgent { request }).await;
    let CommandResult::OkWith(CommandValue::Json(value)) = reply else {
        panic!("question level read failed: {reply:?}");
    };
    serde_json::from_str(&value).unwrap()
}

/// The title sentinel `phux-ask[q1]:Deploy to prod??s=Yes|No|Hold` parses into
/// an `Asked`; the hook then reporting the same question emits nothing new.
#[test]
fn ask_title_sentinel_emits_asked_and_a_repeating_hook_does_not_re_emit() {
    run_local(async {
        let (server, release) = gated_server(
            "printf '\\033]2;phux-ask[q1]:Deploy to prod??s=Yes|No|Hold\\007'; sleep 60",
        );
        let (mut stream, _) = connect_as(&server.socket, "agent-asked", full_caps()).await;
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

        let by_handle = current_questions(
            &mut stream,
            QuestionTarget::Handle {
                resource: pane.clone(),
            },
        )
        .await;
        assert!(
            matches!(by_handle.questions.as_slice(), [CurrentQuestion::PtyEvidence {
            source: PtyQuestionSource::Sentinel, id, ..
        }] if id == "q1")
        );
        let hook = report_asked(pane, "q1", "Deploy to prod?", &["Yes", "No", "Hold"]);
        let (result, duplicates) = asked_before_reply(&mut stream, 9, hook).await;
        assert_eq!(result, CommandResult::Ok, "the hook still owns the ask");
        assert!(
            duplicates.is_empty(),
            "one question is told once: {duplicates:?}"
        );

        drop(stream);
        let (mut reconnected, _) =
            connect_as(&server.socket, "question-reconnect", full_caps()).await;
        let by_key = current_questions(
            &mut reconnected,
            QuestionTarget::Key {
                authority: by_handle.authority,
                resource: by_handle.resource,
            },
        )
        .await;
        assert!(
            matches!(by_key.questions.as_slice(), [CurrentQuestion::PtyEvidence {
            source: PtyQuestionSource::Hook, id, ..
        }] if id == "q1")
        );
        drop(reconnected);
        server.stop().await;
    });
}

#[test]
fn report_asked_emits_asked_and_rejects_an_empty_question() {
    run_local(async {
        let server = SpawnedServer::seeded_in(TempDir::new().unwrap(), "demo", sh("sleep 600"));
        let (mut stream, _) = connect_as(&server.socket, "agent-asked", full_caps()).await;
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
        server.stop().await;
    });
}
