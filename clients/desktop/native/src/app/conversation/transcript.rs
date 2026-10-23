//! Transcript rows and the labels screen readers announce for them.
//!
//! Your prompts sit in quiet bubbles at the trailing edge; the agent's
//! prose owns the reading column; thinking and tool calls
//! fold to one line and open on demand.

use gpui::{ClickEvent, relative};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;

use super::super::pane_identity::Harness;
use super::*;

pub(super) const OMITTED_NOTICE: &str =
    "Earlier entries omitted from the bounded native transcript.";
pub(super) const WAITING_NOTICE: &str = "Waiting for conversation entries. Transcript sharing may be disabled; lifecycle events remain visible here.";

/// What a foldable row runs when its header is clicked.
pub(super) type Toggle = Box<dyn Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>;

/// What every row in one transcript shares.
pub(super) struct Style<'a> {
    pub colors: &'a Palette,
    pub look: &'a Look,
    pub mono: &'a SharedString,
    pub harness: Harness,
    pub provider: &'a str,
}

impl Style<'_> {
    fn muted(&self) -> gpui::Hsla {
        from_hex(&self.colors.muted)
    }
}

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
        .gap(px(6.))
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
        .scrollable(false)
        .stream_fade(!entry.is_final),
    )
}

/// A tool call's command summary on one line; the whole summary reaches
/// screen readers.
pub(super) fn tool_summary(tool: &TranscriptTool, style: &Style<'_>) -> gpui::Stateful<gpui::Div> {
    readable("tool-summary", tool.summary.clone())
        .min_w_0()
        .flex_1()
        .truncate()
        .text_xs()
        .font_family(style.mono.clone())
        .text_color(style.muted())
        .child(first_line(&tool.summary).unwrap_or_default().to_owned())
}

/// A tool call's output, once there is any, in the terminal font.
pub(super) fn tool_output(
    tool: &TranscriptTool,
    style: &Style<'_>,
) -> Option<gpui::Stateful<gpui::Div>> {
    (!tool.output.is_empty()).then(|| {
        readable_lines("tool-output", &tool.output)
            .text_xs()
            .font_family(style.mono.clone())
            .text_color(style.look.ink.opacity(0.85))
    })
}

/// Whether a row folds: thinking with text, a tool call with something
/// beyond its header.
pub(super) fn foldable(entry: &TranscriptEntry) -> bool {
    match (&entry.tool, entry.role) {
        (Some(tool), _) => !tool.output.is_empty() || !entry.text.trim().is_empty(),
        (None, TranscriptRole::Thinking) => !entry.text.trim().is_empty(),
        _ => false,
    }
}

/// A foldable row's state before anyone clicks it: live thinking and a
/// failed call are open, everything else is folded.
pub(super) fn opens_by_default(entry: &TranscriptEntry) -> bool {
    match &entry.tool {
        Some(tool) => tool.status == ToolStatus::Error,
        None => entry.role == TranscriptRole::Thinking && !entry.is_final,
    }
}

/// A lucide glyph for a tool by what its name says it does.
pub(super) fn tool_icon(name: &str) -> IconName {
    let name = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| name.contains(word));
    if has(&["bash", "shell", "exec", "command", "terminal", "run"]) {
        IconName::SquareTerminal
    } else if has(&["edit", "write", "patch", "apply", "replace"]) {
        IconName::Pencil
    } else if has(&["grep", "search", "find", "glob", "ls"]) {
        IconName::Search
    } else if has(&["read", "view", "cat", "open"]) {
        IconName::FileText
    } else if has(&["web", "fetch", "http", "url", "browse"]) {
        IconName::Globe
    } else {
        IconName::Wrench
    }
}

fn chevron(open: bool, style: &Style<'_>) -> Icon {
    Icon::new(if open {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .size(px(12.))
    .text_color(style.muted())
}

/// One transcript entry. `open` folds thinking and tool rows, `lead` marks
/// the first reply of a run so it carries the harness mark, and `toggle`
/// runs when a foldable header is clicked.
pub(super) fn transcript_row(
    entry: &TranscriptEntry,
    open: bool,
    lead: bool,
    style: &Style<'_>,
    toggle: Toggle,
) -> gpui::Stateful<gpui::Div> {
    let selector = format!("conversation-entry-{}", entry.id);
    let row = div()
        .id(SharedString::from(format!(
            "conversation-entry-{}",
            entry.id
        )))
        .debug_selector(move || selector.clone())
        .role(gpui::Role::ListItem)
        .aria_label(entry_label(entry))
        .w_full()
        .flex()
        .flex_col();
    match (&entry.tool, entry.role) {
        (Some(tool), _) => row.child(tool_card(entry, tool, open, style, toggle)),
        (None, TranscriptRole::User) => row.items_end().child(user_bubble(entry, style)),
        (None, TranscriptRole::Assistant) => row.py(px(6.)).child(reply(entry, lead, style)),
        (None, TranscriptRole::Thinking) => row.child(thinking(entry, open, style, toggle)),
        (None, TranscriptRole::Tool | TranscriptRole::System) => row.child(activity(entry, style)),
    }
}

fn user_bubble(entry: &TranscriptEntry, style: &Style<'_>) -> gpui::Div {
    div()
        .max_w(relative(0.86))
        .px(px(12.))
        .py(px(8.))
        .rounded(corner(style.look, 10.))
        .bg(style.look.ink.opacity(0.055))
        .text_color(style.look.ink)
        .child(
            entry_body(entry)
                .text_size(px(15.))
                .line_height(relative(1.45)),
        )
        .children(
            entry
                .truncated
                .then(|| div().text_xs().text_color(style.muted()).child("truncated")),
        )
}

fn reply(entry: &TranscriptEntry, lead: bool, style: &Style<'_>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .when(lead, |reply| reply.pt(px(2.)))
        .child(
            entry_body(entry)
                .text_size(px(15.))
                .line_height(relative(1.45))
                .text_color(style.look.ink),
        )
        .when(!entry.is_final || entry.truncated, |reply| {
            reply.child(
                div()
                    .text_xs()
                    .text_color(style.muted())
                    .child(if entry.is_final {
                        "truncated"
                    } else {
                        "streaming…"
                    }),
            )
        })
}

pub(super) fn fold_header(
    id: String,
    open: bool,
    foldable: bool,
    label: String,
    toggle: Toggle,
) -> gpui::Stateful<gpui::Div> {
    let header = div().id(SharedString::from(id));
    if foldable {
        header
            .role(gpui::Role::Button)
            .aria_label(label)
            .aria_expanded(open)
            // GPUI maps unmodified Enter/Space press+release to on_click for
            // focused elements. Both flags are required for Tab traversal.
            .focusable()
            .tab_stop(true)
            .focus_visible(|style| style.bg(gpui::hsla(0., 0., 0.5, 0.2)))
            .cursor_pointer()
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                toggle(event, window, cx);
            })
    } else {
        header
    }
}

fn thinking(entry: &TranscriptEntry, open: bool, style: &Style<'_>, toggle: Toggle) -> AnyElement {
    let live = !entry.is_final;
    let can_fold = foldable(entry);
    let label: AnyElement = if live {
        ShimmerText::new("Thinking…")
            .id(SharedString::from(format!(
                "conversation-thinking-{}",
                entry.id
            )))
            .into_any_element()
    } else {
        div().child("Thought").into_any_element()
    };
    let header = fold_header(
        format!("conversation-fold-{}", entry.id),
        open,
        can_fold,
        if open {
            "Hide thinking"
        } else {
            "Show thinking"
        }
        .into(),
        toggle,
    )
    .flex()
    .items_center()
    .gap(px(6.))
    .text_size(px(12.))
    .text_color(style.muted())
    .hover(|header| header.text_color(style.look.ink))
    .child(
        Icon::new(IconName::Brain)
            .size(px(13.))
            .text_color(style.muted()),
    )
    .child(label)
    .when(can_fold, |header| header.child(chevron(open, style)));
    gpui_kit::component::collapsible::Collapsible::new()
        .open(open && can_fold)
        .child(header)
        .content(
            div()
                .mt(px(6.))
                .ml(px(6.))
                .pl(px(12.))
                .border_l_2()
                .border_color(style.look.divider)
                .text_sm()
                .text_color(style.muted())
                .child(entry_body(entry)),
        )
        .into_any_element()
}

fn tool_card(
    entry: &TranscriptEntry,
    tool: &TranscriptTool,
    open: bool,
    style: &Style<'_>,
    toggle: Toggle,
) -> AnyElement {
    let can_fold = foldable(entry);
    let ink = style.look.ink;
    let (status, edge): (AnyElement, gpui::Hsla) = match tool.status {
        ToolStatus::Running => (
            Spinner::new()
                .xsmall()
                .color(from_hex(&style.colors.theme.accent))
                .into_any_element(),
            from_hex(&style.colors.theme.accent).opacity(0.35),
        ),
        ToolStatus::Ok => (
            Icon::new(IconName::CircleCheck)
                .size(px(13.))
                .text_color(style.muted())
                .into_any_element(),
            style.look.border,
        ),
        ToolStatus::Error => (
            Icon::new(IconName::CircleX)
                .size(px(13.))
                .text_color(from_hex(&style.colors.theme.danger))
                .into_any_element(),
            from_hex(&style.colors.theme.danger).opacity(0.45),
        ),
    };
    let header = fold_header(
        format!("conversation-fold-{}", entry.id),
        open,
        can_fold,
        format!(
            "{} {} output",
            if open { "Hide" } else { "Show" },
            tool.name
        ),
        toggle,
    )
    .flex()
    .items_center()
    .gap(px(8.))
    .min_w_0()
    .px(px(10.))
    .py(px(4.))
    .when(can_fold, |header| {
        header.hover(|header| header.bg(ink.opacity(0.04)))
    })
    .child(
        Icon::new(tool_icon(&tool.name))
            .size(px(13.))
            .text_color(style.muted()),
    )
    .child(
        div()
            .flex_shrink_0()
            .text_size(px(12.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(ink)
            .child(tool.name.clone()),
    )
    .child(tool_summary(tool, style))
    .child(div().flex_shrink_0().child(status))
    .when(can_fold, |header| header.child(chevron(open, style)));
    let body = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(10.))
        .py(px(8.))
        .border_t_1()
        .border_color(style.look.divider)
        .bg(ink.opacity(0.03))
        .when(!entry.text.trim().is_empty(), |body| {
            body.child(readable_lines("body", &entry.text).text_color(ink))
        })
        .children(tool_output(tool, style));
    div()
        .w_full()
        .rounded(corner(style.look, 8.))
        .when(tool.status == ToolStatus::Error, |card| card.border_1())
        .border_color(edge)
        .when(open || tool.status == ToolStatus::Error, |card| {
            card.bg(ink.opacity(0.02))
        })
        .overflow_hidden()
        .child(
            gpui_kit::component::collapsible::Collapsible::new()
                .open(open && can_fold)
                .child(header)
                .content(body),
        )
        .into_any_element()
}

fn activity(entry: &TranscriptEntry, style: &Style<'_>) -> gpui::Div {
    let notification = entry.id.starts_with("notification:");
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_color(if notification {
            from_hex(&style.colors.theme.warning)
        } else {
            style.muted()
        })
        .child(
            Icon::new(if notification {
                IconName::CircleAlert
            } else {
                IconName::Info
            })
            .size(px(12.)),
        )
        .child(readable_lines("body", &entry.text).text_xs())
}
