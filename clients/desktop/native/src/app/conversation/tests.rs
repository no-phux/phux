//! Conversation identity, receipts, prompt capabilities, and what screen
//! readers announce for the transcript.

use super::receipts::resolve_prompt_receipt;
use super::transcript::{entry_body, entry_label, tool_details};
use super::writes::prompt_available;
use super::*;
use phux_protocol::native_agent::{NativeCapabilities, NativeQuestion};

#[test]
fn native_binding_survives_daemon_rotation_not_resource_or_activation_replacement() {
    let binding = Binding {
        server: "first-incarnation".into(),
        terminal: ResourceId::local(1),
        session: ResourceId::local(2),
        activation: Some("durable-sdk-activation".into()),
        client: std::sync::Weak::new(),
    };
    let mut resumed = binding.clone();
    resumed.server = "next-incarnation".into();
    assert!(binding == resumed);
    resumed.activation = Some("replacement-sdk-activation".into());
    assert!(binding != resumed);
    resumed.activation.clone_from(&binding.activation);
    resumed.session = ResourceId::local(3);
    assert!(binding != resumed);
    resumed.session = binding.session.clone();
    resumed.terminal = ResourceId::local(4);
    assert!(binding != resumed);
    let mut attached = binding;
    attached.activation = None;
    resumed = attached.clone();
    resumed.server = "next-incarnation".into();
    assert!(
        attached != resumed,
        "an unowned TUI has no durable SDK identity"
    );
}

#[test]
fn queued_and_unknown_prompt_waits_for_correlated_provider_admission() {
    let mut pending = Some((42, "draft".into()));
    let mut delivered = None;
    for disposition in [NativeDisposition::Queued, NativeDisposition::Unknown] {
        resolve_prompt_receipt(&mut pending, &mut delivered, 42, disposition);
        assert!(
            pending.is_some(),
            "an unresolved SDK input must not become resubmittable"
        );
        assert!(
            delivered.is_none(),
            "queueing or ambiguity cannot consume the draft"
        );
    }
    resolve_prompt_receipt(
        &mut pending,
        &mut delivered,
        41,
        NativeDisposition::Admitted,
    );
    assert!(
        pending.is_some(),
        "another operation cannot consume this draft"
    );
    resolve_prompt_receipt(
        &mut pending,
        &mut delivered,
        42,
        NativeDisposition::Admitted,
    );
    assert!(pending.is_none());
    assert_eq!(delivered.as_deref(), Some("draft"));
}

fn entry(role: TranscriptRole, text: &str) -> TranscriptEntry {
    TranscriptEntry {
        id: "entry".into(),
        role,
        text: text.into(),
        truncated: false,
        is_final: true,
        tool: None,
    }
}

fn tool(summary: &str, output: &str) -> TranscriptTool {
    TranscriptTool {
        name: "bash".into(),
        call_id: "entry".into(),
        summary: summary.into(),
        status: ToolStatus::Error,
        output: output.into(),
    }
}

/// The role and accessibility properties GPUI reports for `element`.
pub(crate) fn a11y(element: &impl gpui::Element) -> (Option<gpui::Role>, gpui::accesskit::Node) {
    let role = element.a11y_role();
    let mut node = gpui::accesskit::Node::new(role.unwrap_or(gpui::Role::Unknown));
    element.write_a11y_info(&mut node);
    (role, node)
}

#[test]
fn rows_name_their_speaker_and_first_line_for_screen_readers() {
    let reply = entry(TranscriptRole::Assistant, "\n  Done.  \nSecond line");
    assert_eq!(entry_label(&reply), "Assistant: Done.");
    assert_eq!(entry_label(&entry(TranscriptRole::User, "hi")), "You: hi");
    assert_eq!(entry_label(&entry(TranscriptRole::System, "")), "Activity");
    let mut live = entry(TranscriptRole::Assistant, "Working");
    live.is_final = false;
    live.truncated = true;
    assert_eq!(
        entry_label(&live),
        "Assistant, streaming, truncated: Working"
    );
    let mut call = entry(TranscriptRole::Tool, "");
    call.tool = Some(tool("ls\n-la", ""));
    assert_eq!(entry_label(&call), "Tool bash, error: ls");
    call.tool = Some(tool("", ""));
    assert_eq!(entry_label(&call), "Tool bash, error");
}

#[test]
fn transcript_rows_are_labeled_list_items() {
    use crate::settings::theme::{default_theme_id, palette, theme_by_id};
    let colors = palette(&theme_by_id(default_theme_id(), &[]));
    let look = Look::of(
        &crate::settings::skin::TERMINAL,
        &colors,
        &crate::settings::default_display(),
    );
    let row = transcript_row(
        &entry(TranscriptRole::User, "Run the tests\nthen push"),
        &colors,
        &look,
        &"Menlo".into(),
    );
    let (role, node) = a11y(&row);
    assert_eq!(role, Some(gpui::Role::ListItem));
    assert_eq!(node.label(), Some("You: Run the tests"));

    let (role, node) = a11y(&conversation_list(vec![row]));
    assert_eq!(role, Some(gpui::Role::List));
    assert_eq!(node.label(), Some("Conversation"));
}

#[test]
fn whole_bodies_and_tool_details_reach_screen_readers() {
    use crate::settings::theme::{default_theme_id, palette, theme_by_id};
    let colors = palette(&theme_by_id(default_theme_id(), &[]));
    for role in [TranscriptRole::User, TranscriptRole::Assistant] {
        let (role, node) = a11y(&entry_body(&entry(role, "Run the tests\nthen push")));
        assert_eq!(role, Some(gpui::Role::Label));
        assert_eq!(node.value(), Some("Run the tests\nthen push"));
    }
    let details = tool_details(&tool("ls -la", "denied\nexit 1"), &colors, &"Menlo".into());
    let values: Vec<_> = details
        .iter()
        .map(|detail| {
            let (role, node) = a11y(detail);
            assert_eq!(role, Some(gpui::Role::Label));
            node.value().map(str::to_owned)
        })
        .collect();
    assert_eq!(
        values,
        [Some("ls -la".to_owned()), Some("denied\nexit 1".to_owned())]
    );
    assert_eq!(
        tool_details(&tool("ls", ""), &colors, &"Menlo".into()).len(),
        1
    );
    for (id, text) in [
        ("conversation-omitted", OMITTED_NOTICE),
        ("conversation-waiting", WAITING_NOTICE),
    ] {
        let (role, node) = a11y(&readable(id, text));
        assert_eq!(role, Some(gpui::Role::Label));
        assert_eq!(node.value(), Some(text));
    }
}

#[test]
fn native_prompts_obey_actual_capabilities_and_open_questions() {
    let mut view = NativeView {
        activation: "actual-activation".into(),
        backend: "acp".into(),
        connected: true,
        capabilities: NativeCapabilities {
            prompt: true.into(),
            interrupt: true.into(),
            questions: true.into(),
            follow_up: false.into(),
        },
        turn_id: None,
        provider_session_id: None,
        questions: vec![],
    };
    assert!(prompt_available(&view, AgentSessionStatus::Idle));
    assert!(!prompt_available(&view, AgentSessionStatus::Working));
    view.capabilities.follow_up = true.into();
    assert!(prompt_available(&view, AgentSessionStatus::Working));
    view.questions.push(NativeQuestion {
        id: "real-question".into(),
        kind: "permission".into(),
        title: "Permission".into(),
        message: "Tool call".into(),
        options: vec![],
        questions: vec![],
        fields: serde_json::json!({}),
    });
    assert!(!prompt_available(&view, AgentSessionStatus::Blocked));
    view.questions.clear();
    view.connected = false;
    assert!(!prompt_available(&view, AgentSessionStatus::Idle));
}

/// A face cut down to its transcript viewport and a composer below it.
struct Viewport {
    following: bool,
}

impl gpui::Render for Viewport {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let jump = (!self.following).then(|| {
            div()
                .debug_selector(|| "conversation-jump".into())
                .w(px(120.))
                .h(px(34.))
        });
        div()
            .size(px(400.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(super::render::transcript_viewport(
                div()
                    .debug_selector(|| "conversation-transcript".into())
                    .flex_1()
                    .min_h_0(),
                jump,
            ))
            .child(
                div()
                    .debug_selector(|| "conversation-composer".into())
                    .h(px(60.)),
            )
    }
}

fn draw(cx: &mut gpui::VisualTestContext) {
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

#[gpui::test]
fn pausing_follow_never_resizes_the_transcript(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Viewport { following: true });
    draw(cx);
    let transcript = cx
        .debug_bounds("conversation-transcript")
        .expect("transcript");
    let composer = cx.debug_bounds("conversation-composer").expect("composer");
    view.update(cx, |view, cx| {
        view.following = false;
        cx.notify();
    });
    draw(cx);
    assert_eq!(
        cx.debug_bounds("conversation-transcript"),
        Some(transcript),
        "the first scroll that pauses follow must not shrink the transcript"
    );
    assert_eq!(cx.debug_bounds("conversation-composer"), Some(composer));
    let jump = cx
        .debug_bounds("conversation-jump")
        .expect("Jump to latest");
    assert!(
        jump.top() >= transcript.top() && jump.bottom() <= transcript.bottom(),
        "Jump to latest floats over the transcript"
    );
}
