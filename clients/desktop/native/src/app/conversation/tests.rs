//! Conversation identity, receipts, prompt capabilities, and what screen
//! readers announce for the transcript.

use super::receipts::resolve_prompt_receipt;
use super::transcript::{
    Style, entry_body, entry_label, fold_header, foldable, opens_by_default, tool_icon,
    tool_output, tool_summary, transcript_row,
};
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
    let mono = "Menlo".into();
    let style = style(&colors, &look, &mono);
    let row = transcript_row(
        &entry(TranscriptRole::User, "Run the tests\nthen push"),
        false,
        false,
        &style,
        Box::new(|_, _, _| {}),
    );
    let (role, node) = a11y(&row);
    assert_eq!(role, Some(gpui::Role::ListItem));
    assert_eq!(node.label(), Some("You: Run the tests"));

    let (role, node) = a11y(&conversation_list(vec![row]));
    assert_eq!(role, Some(gpui::Role::List));
    assert_eq!(node.label(), Some("Conversation"));
    for role in [
        TranscriptRole::Assistant,
        TranscriptRole::Thinking,
        TranscriptRole::System,
    ] {
        let mut call = entry(role, "Checked the tree\nall clean");
        call.id = format!("{role:?}");
        let (_, node) = a11y(&transcript_row(
            &call,
            true,
            true,
            &style,
            Box::new(|_, _, _| {}),
        ));
        assert_eq!(
            node.label().map(str::to_owned),
            Some(entry_label(&call)),
            "{role:?} rows keep their spoken name whatever they look like"
        );
    }
}

fn style<'a>(
    colors: &'a crate::settings::theme::Palette,
    look: &'a Look,
    mono: &'a SharedString,
) -> Style<'a> {
    Style {
        colors,
        look,
        mono,
        harness: crate::app::pane_identity::Harness::Claude,
        provider: "Claude Code",
    }
}

#[test]
fn live_thinking_and_failed_calls_open_while_quiet_rows_fold() {
    let mut thought = entry(TranscriptRole::Thinking, "Weighing the options");
    assert!(foldable(&thought) && !opens_by_default(&thought));
    thought.is_final = false;
    assert!(
        opens_by_default(&thought),
        "live reasoning reads as it streams"
    );
    assert!(!foldable(&entry(TranscriptRole::Thinking, "  ")));
    assert!(!foldable(&entry(TranscriptRole::Assistant, "Done")));
    let mut call = entry(TranscriptRole::Tool, "");
    call.tool = Some(tool("ls", ""));
    assert!(
        !foldable(&call),
        "a call with nothing beyond its header stays a line"
    );
    call.tool = Some(tool("ls", "denied"));
    assert!(
        foldable(&call) && opens_by_default(&call),
        "a failed call shows why"
    );
    call.tool.as_mut().unwrap().status = ToolStatus::Ok;
    assert!(!opens_by_default(&call));
}

#[test]
fn only_a_header_that_folds_is_announced_as_a_button() {
    let (role, node) = a11y(&fold_header(
        "fold".into(),
        true,
        true,
        "Hide thinking".into(),
        Box::new(|_, _, _| {}),
    ));
    assert_eq!(role, Some(gpui::Role::Button));
    assert_eq!(node.label(), Some("Hide thinking"));
    assert_eq!(node.is_expanded(), Some(true));
    let (role, _) = a11y(&fold_header(
        "quiet".into(),
        false,
        false,
        "Show thinking".into(),
        Box::new(|_, _, _| {}),
    ));
    assert_eq!(role, None, "an empty live thought has nothing to open");
}

#[test]
fn fold_choices_follow_transcript_retention() {
    let mut entries: std::collections::VecDeque<_> = (0..256)
        .map(|index| {
            let mut item = entry(TranscriptRole::Thinking, "Thought");
            item.id = index.to_string();
            item
        })
        .collect();
    let mut folds: HashMap<_, _> = [
        ("0".into(), true),
        ("1".into(), false),
        ("255".into(), true),
    ]
    .into_iter()
    .collect();
    super::render::retain_folds(&mut folds, &entries);
    assert_eq!(folds.len(), 3);
    entries.pop_front();
    let mut next = entry(TranscriptRole::Thinking, "Later thought");
    next.id = "256".into();
    entries.push_back(next);
    super::render::retain_folds(&mut folds, &entries);
    assert!(!folds.contains_key("0"), "evicted choices are forgotten");
    assert_eq!(folds.get("1"), Some(&false), "explicit folds survive");
    assert_eq!(folds.get("255"), Some(&true), "explicit opens survive");
    entries.clear();
    super::render::retain_folds(&mut folds, &entries);
    assert!(folds.is_empty(), "replacement replay clears old choices");
}

struct FoldKeyboardFixture {
    anchor: gpui::FocusHandle,
    open: [bool; 2],
}

impl gpui::Render for FoldKeyboardFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = crate::settings::default_display();
        let colors = crate::settings::theme::palette(&crate::settings::theme::theme_by_id(
            &prefs.theme_id,
            &[],
        ));
        let look = Look::of(&crate::settings::skin::TERMINAL, &colors, &prefs);
        let mono = prefs.font_family.into();
        let style = style(&colors, &look, &mono);
        let thought = entry(TranscriptRole::Thinking, "Finished thinking");
        let mut call = entry(TranscriptRole::Tool, "");
        call.id = "call".into();
        let mut details = tool("ls", "result");
        details.status = ToolStatus::Ok;
        call.tool = Some(details);
        let mut empty = entry(TranscriptRole::Thinking, "");
        empty.id = "empty".into();
        div()
            .size_full()
            .tab_group()
            .child(
                div()
                    .id("anchor")
                    .track_focus(&self.anchor)
                    .tab_stop(true)
                    .child("Start"),
            )
            .child(transcript_row(
                &empty,
                false,
                false,
                &style,
                Box::new(|_, _, _| panic!("empty thoughts must not receive activation")),
            ))
            .children([thought, call].iter().enumerate().map(|(index, entry)| {
                transcript_row(
                    entry,
                    self.open[index],
                    false,
                    &style,
                    Box::new(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                        this.open[index] = !this.open[index];
                        cx.notify();
                    })),
                )
            }))
    }
}

#[gpui::test]
fn tab_and_enter_or_space_operate_folded_thinking_and_tool_rows(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_kit::init);
    let mut content = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| FoldKeyboardFixture {
            anchor: cx.focus_handle(),
            open: [false; 2],
        });
        content = Some(view.clone());
        gpui_kit::base::Root::new(view, window, cx)
    });
    let view = content.unwrap();
    draw(cx);
    cx.update(|window, cx| view.read(cx).anchor.clone().focus(window, cx));
    for index in 0..2 {
        cx.simulate_keystrokes("tab");
        draw(cx);
        cx.update(|window, cx| {
            assert!(
                !view.read(cx).anchor.is_focused(window),
                "Tab must leave the anchor"
            );
            assert!(window.focused(cx).is_some(), "Tab must reach a fold header");
        });
        press_fold_key(cx, "enter");
        draw(cx);
        assert!(
            view.read_with(cx, |view, _| view.open[index]),
            "row {index} opens on Enter"
        );
        press_fold_key(cx, "space");
        draw(cx);
        assert!(!view.read_with(cx, |view, _| view.open[index]));
    }
}

fn press_fold_key(cx: &mut gpui::VisualTestContext, key: &'static str) {
    use gpui::InputEvent as _;
    let keystroke = gpui::Keystroke::parse(key).unwrap();
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(gpui::KeyUpEvent { keystroke }.to_platform_input(), cx);
    });
}

#[test]
fn tool_glyphs_follow_what_the_tool_does() {
    use gpui_kit::assets::IconName;
    assert_eq!(tool_icon("Bash"), IconName::SquareTerminal);
    assert_eq!(tool_icon("edit_file"), IconName::Pencil);
    assert_eq!(tool_icon("Grep"), IconName::Search);
    assert_eq!(tool_icon("read"), IconName::FileText);
    assert_eq!(tool_icon("WebFetch"), IconName::Globe);
    assert_eq!(tool_icon("todo"), IconName::Wrench);
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
    let look = Look::of(
        &crate::settings::skin::TERMINAL,
        &colors,
        &crate::settings::default_display(),
    );
    let mono = "Menlo".into();
    let style = style(&colors, &look, &mono);
    let call = tool("ls -la\n--color", "denied\nexit 1");
    let mut details = vec![tool_summary(&call, &style)];
    details.extend(tool_output(&call, &style));
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
        [
            Some("ls -la\n--color".to_owned()),
            Some("denied\nexit 1".to_owned())
        ],
        "the one-line summary still speaks the whole command"
    );
    assert!(tool_output(&tool("ls", ""), &style).is_none());
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
