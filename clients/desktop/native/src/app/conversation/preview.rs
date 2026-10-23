//! Isolated inventory for rendering the real compose → agent → reply surface.

use phux_client_core::session::{AgentStreamInfo, agent_stream::AgentRecordMode};
use phux_client_runtime::control::{AgentPublication, AgentSessionInfo};

use super::*;

pub(crate) fn seed(connection: &mut Connection, parent: ResourceId) {
    let resource = ResourceId::local(10);
    connection.publish_test_conversation(
        &AgentPublication {
            resource_id: resource.clone(),
            stream: AgentStreamInfo {
                stream_id: phux_protocol::ids::StreamId::new(10).unwrap(),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(10).unwrap(),
                last_seq: 1,
            },
            mode: AgentRecordMode::Replace,
            records: Vec::new(),
        },
        &AgentSessionInfo {
            parent: Some(parent),
            provider: Some("opencode".into()),
            ..Default::default()
        },
    );
    let conversation = connection.conversations.get_mut(&resource).unwrap();
    conversation.state.status = AgentSessionStatus::Done;
    conversation.native = Some(NativeView {
        activation: "design-preview".into(),
        backend: "acp".into(),
        connected: true,
        capabilities: phux_protocol::native_agent::NativeCapabilities {
            prompt: true.into(),
            interrupt: true.into(),
            questions: true.into(),
            follow_up: true.into(),
        },
        turn_id: None,
        provider_session_id: Some("preview-session".into()),
        questions: Vec::new(),
    });
    conversation.entries = [
        entry("request", TranscriptRole::User, "Find out why the CI tests are failing and fix the underlying issue."),
        entry("progress", TranscriptRole::Assistant, "The parser was treating an empty response as a successful result. I’ve corrected the shared path and added a regression test."),
        TranscriptEntry {
            tool: Some(TranscriptTool {
                name: "Run tests".into(), call_id: "test".into(), summary: "cargo test -p phux-client-core".into(),
                status: ToolStatus::Ok, output: "148 tests passed. No failures.".into(),
            }),
            ..entry("tests", TranscriptRole::Tool, "")
        },
        entry("answer", TranscriptRole::Assistant, "**Fixed and verified.**\n\nEmpty responses now produce an explicit error instead of silently continuing. The existing callers use the same validation, so the fix covers both connection paths.\n\nAll 148 tests pass, including the new regression."),
    ].into();
    conversation.revision += 1;
}

fn entry(id: &str, role: TranscriptRole, text: &str) -> TranscriptEntry {
    TranscriptEntry {
        id: id.into(),
        role,
        text: text.into(),
        truncated: false,
        is_final: true,
        tool: None,
    }
}

pub(crate) fn scene(desk: &mut Desk, name: &str) {
    let Some(conversation) = desk.link.as_mut().and_then(|link| {
        link.connection
            .conversations
            .get_mut(&ResourceId::local(10))
    }) else {
        return;
    };
    let view = conversation.native.as_mut().unwrap();
    view.connected = name != "conversation-disconnected";
    view.questions.clear();
    view.turn_id = None;
    conversation.state.status = AgentSessionStatus::Done;
    if name == "conversation-working" {
        conversation.state.status = AgentSessionStatus::Working;
        view.turn_id = Some("preview-turn".into());
    }
    if name == "conversation-needs-you" {
        conversation.state.status = AgentSessionStatus::Blocked;
        view.questions
            .push(phux_protocol::native_agent::NativeQuestion {
                id: "permission".into(),
                kind: "permission".into(),
                title: "Run the full test suite?".into(),
                message: "The focused tests pass. The full suite will take a few minutes.".into(),
                options: [("allow_once", "Run tests"), ("reject_once", "Not now")]
                    .into_iter()
                    .map(|(id, label)| phux_protocol::native_agent::NativeOption {
                        id: id.into(),
                        label: label.into(),
                        description: None,
                    })
                    .collect(),
                questions: Vec::new(),
                fields: serde_json::json!({}),
            });
    }
    conversation.revision += 1;
}
