//! Transcript rows and the labels screen readers announce for them.

use super::*;

pub(super) const OMITTED_NOTICE: &str =
    "Earlier entries omitted from the bounded native transcript.";
pub(super) const WAITING_NOTICE: &str = "Waiting for conversation entries. Transcript sharing may be disabled; lifecycle events remain visible here.";

/// A container screen readers announce as static text. GPUI only reports
/// nodes that carry both an id and a role, and plain string children carry
/// neither, so visible text that matters is mirrored into a `Label` value.
pub(super) fn readable(
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    div().id(id.into()).role(gpui::Role::Label).aria_value(text)
}

/// The transcript as one named list whose children are the entry rows.
pub(super) fn conversation_list(
    rows: impl IntoIterator<Item = gpui::Stateful<gpui::Div>>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("conversation-list")
        .role(gpui::Role::List)
        .aria_label("Conversation")
        .flex()
        .flex_col()
        .gap(px(12.))
        .children(rows)
}

pub(super) fn speaker(role: TranscriptRole) -> &'static str {
    match role {
        TranscriptRole::User => "You",
        TranscriptRole::Assistant => "Assistant",
        TranscriptRole::Thinking => "Thinking",
        TranscriptRole::Tool => "Tool",
        TranscriptRole::System => "Activity",
    }
}

pub(super) fn tool_status(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Running => "running",
        ToolStatus::Ok => "ok",
        ToolStatus::Error => "error",
    }
}

/// The first non-empty line of `text`, trimmed.
pub(super) fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

/// What a screen reader announces when it lands on a row: the speaker (or
/// tool, state and command) plus the first line. The full body, summary and
/// output are separate `Label` children, so this stays a short name.
pub(super) fn entry_label(entry: &TranscriptEntry) -> String {
    if let Some(tool) = &entry.tool {
        let name = format!("Tool {}, {}", tool.name, tool_status(tool.status));
        return match first_line(&tool.summary) {
            Some(summary) => format!("{name}: {summary}"),
            None => name,
        };
    }
    let mut name = speaker(entry.role).to_owned();
    if !entry.is_final {
        name.push_str(", streaming");
    }
    if entry.truncated {
        name.push_str(", truncated");
    }
    match first_line(&entry.text) {
        Some(first) => format!("{name}: {first}"),
        None => name,
    }
}

/// Plain text set one visual line per `div`, exposed as one `Label` holding
/// the whole text.
pub(super) fn readable_lines(id: &'static str, text: &str) -> gpui::Stateful<gpui::Div> {
    readable(id, text.to_owned())
        .text_sm()
        .children(text.lines().map(|line| div().child(line.to_owned())))
}

/// The entry body: markdown prose for the assistant and thinking, plain lines
/// otherwise. Screen readers get the full text either way.
pub(super) fn entry_body(entry: &TranscriptEntry) -> gpui::Stateful<gpui::Div> {
    if !matches!(
        entry.role,
        TranscriptRole::Assistant | TranscriptRole::Thinking
    ) {
        return readable_lines("body", &entry.text);
    }
    readable("body", entry.text.clone()).child(
        TextView::markdown(
            SharedString::from(format!("conversation-prose-{}", entry.id)),
            entry.text.clone(),
        )
        .selectable(true)
        .scrollable(false),
    )
}

/// A tool call's command summary and, once there is any, its output.
pub(super) fn tool_details(
    tool: &TranscriptTool,
    colors: &Palette,
    mono: &SharedString,
) -> Vec<gpui::Stateful<gpui::Div>> {
    let mut details = vec![
        readable("tool-summary", tool.summary.clone())
            .text_sm()
            .text_color(from_hex(&colors.muted))
            .child(tool.summary.clone()),
    ];
    if !tool.output.is_empty() {
        details.push(readable_lines("tool-output", &tool.output).font_family(mono.clone()));
    }
    details
}

/// One transcript entry; tool output sets in the terminal font `mono`.
pub(super) fn transcript_row(
    entry: &TranscriptEntry,
    colors: &Palette,
    look: &Look,
    mono: &SharedString,
) -> gpui::Stateful<gpui::Div> {
    let label = speaker(entry.role);
    let mut row = div()
        .id(SharedString::from(format!(
            "conversation-entry-{}",
            entry.id
        )))
        .role(gpui::Role::ListItem)
        .aria_label(entry_label(entry))
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(10.))
        .rounded(corner(look, 8.))
        .bg(from_hex(&colors.raised).opacity(0.55))
        .child(
            div()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child(format!(
                    "{label}{}{}",
                    if entry.is_final { "" } else { " · streaming" },
                    if entry.truncated { " · truncated" } else { "" }
                )),
        )
        .child(entry_body(entry));
    if let Some(tool) = &entry.tool {
        row = row
            .child(
                div()
                    .text_sm()
                    .child(format!("{} · {}", tool.name, tool_status(tool.status))),
            )
            .children(tool_details(tool, colors, mono));
    }
    row
}
