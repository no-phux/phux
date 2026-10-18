//! The conversation face: header, transcript, composer and controls.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;

use super::super::pane_identity::Harness;
use super::super::ui::status::{agent_swatch, harness_mark};
use super::super::ui::{badge, pill};
use super::transcript::{Style, Toggle, foldable, opens_by_default};
use super::*;
use crate::connection::AgentState;

/// Prose reads best in a column; a wide pane centers it.
const READING_WIDTH: f32 = 820.;

/// The swatch state a conversation's lifecycle wears.
fn swatch_state(status: AgentSessionStatus) -> AgentState {
    match status {
        AgentSessionStatus::Working => AgentState::Working,
        AgentSessionStatus::Blocked => AgentState::Blocked,
        AgentSessionStatus::Done => AgentState::Done,
        AgentSessionStatus::Idle => AgentState::Idle,
        AgentSessionStatus::Ended | AgentSessionStatus::Unknown => AgentState::Unknown,
    }
}

pub(crate) fn render(
    desk: &mut Desk,
    placement: &str,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    desk.prepare_conversation(placement, window, cx);
    let look = desk.look();
    let mono = SharedString::from(desk.prefs.font_family.clone());
    let writable = desk.conversation_writable(placement, false);
    let stopping = desk.conversation_writable(placement, true);
    let origin = desk
        .placement(placement)
        .map(|item| desk.placement_origin(item).harness());
    let Some(face) = desk.conversation_faces.get_mut(placement) else {
        return unavailable(placement, cx);
    };
    let conversation = desk
        .link
        .as_ref()
        .and_then(|link| link.connection.conversations.get(&face.binding.session));
    let Some(conversation) = conversation else {
        return unavailable(placement, cx);
    };
    let blocked = conversation.state.status == AgentSessionStatus::Blocked;
    // Measure the previous frame before appending; do not jump if the reader scrolled up.
    if let Some(submitted) = face.delivered.take() {
        face.input.update(cx, |input, cx| {
            if input.value().as_str() == submitted {
                input.set_value("", window, cx);
            }
        });
    }
    if conversation.revision != face.revision {
        let max =
            (face.scroll.content_size().height - face.scroll.bounds().size.height).max(px(0.));
        face.follow = face.follow && -face.scroll.offset().y >= max - px(32.);
        face.revision = conversation.revision;
        if face.follow {
            let scroll = face.scroll.clone();
            window.on_next_frame(move |window, _| {
                scroll.set_offset(point(px(0.), px(-1_000_000.)));
                window.refresh();
            });
        }
    }
    let disconnected = conversation
        .native
        .as_ref()
        .is_some_and(|view| !view.connected);
    let state = if conversation.closed {
        "closed · read-only"
    } else if disconnected {
        "ACP disconnected · read-only"
    } else {
        match conversation.state.status {
            AgentSessionStatus::Working => "working",
            AgentSessionStatus::Blocked => "needs you",
            AgentSessionStatus::Done => "finished",
            AgentSessionStatus::Ended => "ended · read-only",
            AgentSessionStatus::Idle => "idle",
            AgentSessionStatus::Unknown => "connected",
        }
    };
    let harness = conversation
        .state
        .provider
        .as_deref()
        .map(Harness::from_slug)
        .filter(|harness| *harness != Harness::Other)
        .or(origin.flatten())
        .unwrap_or(Harness::Other);
    let provider = conversation
        .state
        .provider
        .clone()
        .filter(|_| harness == Harness::Other)
        .unwrap_or_else(|| harness.label().to_owned());
    let native = conversation.native.is_some();
    let live = !conversation.closed && conversation.state.status != AgentSessionStatus::Ended;
    let swatch = swatch_state(conversation.state.status);
    let native_identity = conversation.native.as_ref().map(|view| {
        format!(
            "{} · AgentSession {} · provider session {}",
            view.backend,
            conversation.resource,
            view.provider_session_id
                .as_deref()
                .unwrap_or("not yet reported by ACP agent"),
        )
    });
    let style = Style {
        colors,
        look: &look,
        mono: &mono,
        harness,
        provider: &provider,
    };
    let mut previous = None;
    let rows = conversation
        .entries
        .iter()
        .map(|entry| {
            let lead = entry.role == TranscriptRole::Assistant && previous != Some(entry.role);
            previous = Some(entry.role);
            let open = foldable(entry)
                && face
                    .folds
                    .get(&entry.id)
                    .copied()
                    .unwrap_or_else(|| opens_by_default(entry));
            transcript::transcript_row(
                entry,
                open,
                lead,
                &style,
                fold_toggle(placement, &entry.id, open, cx),
            )
        })
        .collect::<Vec<_>>();
    let omitted = conversation.omitted;
    let input = face.input.clone();
    let empty = input.read(cx).value().trim().is_empty();
    let scroll = face.scroll.clone();
    let pending = face.pending.is_some();
    let error = face.error.clone();
    let notice = face.notice.clone();
    let has_rows = !rows.is_empty();
    let question_rows = questions::render(
        face,
        conversation.native.as_ref().filter(|_| live),
        !conversation.closed
            && desk.link.as_ref().is_some_and(|link| {
                link.connection.status == Status::Attached
                    && conversation.epoch == link.connection.connection_epoch()
            }),
        placement,
        &look,
        cx,
    );
    // Native drafts are local: a provider question or delivery fence blocks Send,
    // not composing the next message.
    let editable = if native { live } else { !pending && writable };
    let working = live && !disconnected && conversation.state.status == AgentSessionStatus::Working;
    let following = face.follow;
    let terminal_id = placement.to_owned();
    let send_id = placement.to_owned();
    let stop_id = placement.to_owned();
    let live_id = placement.to_owned();
    let scroll_id = placement.to_owned();
    let focus_id = placement.to_owned();
    let muted = from_hex(&colors.muted);
    let hint = if native && blocked {
        "Respond to the provider request above · first response across viewers wins"
    } else if blocked {
        "Attached TUI needs you · Open Terminal to answer approvals or permissions"
    } else if writable {
        "Enter sends · Shift+Enter adds a line"
    } else {
        "Read-only until this exact terminal and agent session are writable"
    };
    let header = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(harness_mark(harness, Some(swatch), 18., &look))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(look.ink)
                                .truncate()
                                .child(provider.clone()),
                        )
                        .child(if native {
                            badge("ACP", &look)
                        } else {
                            pill(
                                "Attached TUI",
                                &format!("{}14", colors.theme.foreground),
                                &colors.muted,
                                &look,
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(muted)
                        .child(super::super::ui::dot(6., from_hex(agent_swatch(swatch))))
                        .child(state),
                ),
        )
        .child(
            Button::new(SharedString::from(format!(
                "conversation-terminal-{placement}"
            )))
            .label("Terminal")
            .icon(IconName::SquareTerminal)
            .ghost()
            .small()
            .tooltip("Show this agent's terminal")
            .on_click(cx.listener(move |desk, _, window, cx| {
                desk.toggle_conversation(&terminal_id, window, cx)
            })),
        );
    let activity =
        if working {
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_xs()
                    .text_color(muted)
                    .child(
                        Spinner::new()
                            .xsmall()
                            .color(from_hex(&colors.theme.accent)),
                    )
                    .child(ShimmerText::new(format!("{provider} is working…")).id(
                        SharedString::from(format!("conversation-working-{placement}")),
                    ))
                    .into_any_element(),
            )
        } else if live && blocked {
            let color = from_hex(agent_swatch(AgentState::Blocked));
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_xs()
                    .text_color(color)
                    .child(Icon::new(IconName::CircleAlert).size(px(13.)))
                    .child(format!("{provider} needs you"))
                    .into_any_element(),
            )
        } else {
            None
        };
    let column = |body: gpui::Div| body.w_full().max_w(px(READING_WIDTH)).mx_auto();
    let composer = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .p(px(8.))
        .rounded(corner(&look, 12.))
        .border_1()
        .border_color(look.border)
        .bg(look.ink.opacity(0.035))
        .child(
            Textarea::new(&input)
                .appearance(false)
                .aria_label("Message")
                .w_full()
                .disabled(!editable),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(hint),
                )
                .child(
                    Button::new(SharedString::from(format!("conversation-stop-{placement}")))
                        .label("Stop · Esc")
                        .icon(IconName::Square)
                        .accessibility_label("Stop agent")
                        .ghost()
                        .small()
                        .disabled(!stopping)
                        .on_click(
                            cx.listener(move |desk, _, _, cx| desk.stop_conversation(&stop_id, cx)),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("conversation-send-{placement}")))
                        .label(if pending { "Sending…" } else { "Send" })
                        .icon(IconName::ArrowUp)
                        .loading(pending)
                        .primary()
                        .small()
                        .disabled(!writable || empty)
                        .on_click(
                            cx.listener(move |desk, _, _, cx| desk.send_conversation(&send_id, cx)),
                        ),
                ),
        );
    let transcript = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .pt(px(4.))
        .pb(px(16.))
        .when(omitted, |list| {
            list.child(
                readable("conversation-omitted", OMITTED_NOTICE)
                    .text_xs()
                    .text_color(muted)
                    .child(OMITTED_NOTICE),
            )
        })
        .when(!has_rows, |list| {
            list.child(waiting(harness, &provider, &look, colors))
        })
        .when(has_rows, |list| list.child(conversation_list(rows)))
        .children(question_rows)
        .children(activity);
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(10.))
        .text_color(from_hex(&colors.theme.foreground))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |desk, _, window, cx| {
                if desk
                    .focused_placement()
                    .map(|placement| placement.id.as_str())
                    != Some(focus_id.as_str())
                    && let Some(tab) = desk.active_tab_id()
                {
                    desk.focus_placement(&tab, &focus_id, window, cx);
                }
            }),
        )
        .child(column(div().child(header)))
        .children(native_identity.map(|identity| {
            column(div())
                .truncate()
                .text_xs()
                .text_color(from_hex(&colors.faint))
                .child(identity)
        }))
        .child(div().h(px(1.)).flex_shrink_0().bg(look.divider))
        .child(transcript_viewport(
            div()
                .id(SharedString::from(format!("conversation-scroll-{placement}")))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .on_scroll_wheel(cx.listener(move |desk, _, _, cx| {
                    if let Some(face) = desk.conversation_faces.get_mut(&scroll_id) {
                        // Explicit reader interaction pauses follow until Jump to latest.
                        face.follow = false;
                        cx.notify();
                    }
                }))
                // Only transcript rows live in the List; notices and question
                // panels sit beside it so every List child is a ListItem.
                .child(column(transcript)),
            // Floats over the transcript: pausing follow must not resize it.
            (!following).then(|| {
                Button::new(SharedString::from(format!("conversation-live-{placement}")))
                    .label("Jump to latest")
                    .icon(IconName::ArrowDown)
                    .small()
                    .rounded(px(999.))
                    .on_click(cx.listener(move |desk, _, _, cx| {
                        if let Some(face) = desk.conversation_faces.get_mut(&live_id) {
                            face.follow = true;
                            face.scroll.set_offset(point(px(0.), px(-1_000_000.)));
                        }
                        cx.notify();
                    }))
            }),
        ))
        .children(error.map(|error| {
            column(div())
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .py(px(6.))
                .rounded(corner(&look, 8.))
                .bg(from_hex(&colors.theme.warning).opacity(0.1))
                .text_sm()
                .text_color(from_hex(&colors.theme.warning))
                .child(Icon::new(IconName::TriangleAlert).size(px(13.)))
                .child(error)
        }))
        .children(notice.map(|notice| column(div()).text_xs().text_color(muted).child(notice)))
        .child(column(composer))
        .into_any_element()
}

/// What a fold header runs: flip that entry's fold away from `open`.
fn fold_toggle(placement: &str, entry: &str, open: bool, cx: &mut Context<Desk>) -> Toggle {
    let (placement, entry) = (placement.to_owned(), entry.to_owned());
    Box::new(cx.listener(move |desk, _, _, cx| {
        if let Some(face) = desk.conversation_faces.get_mut(&placement) {
            face.folds.insert(entry.clone(), !open);
            cx.notify();
        }
    }))
}

/// The empty transcript: the harness that will speak here, and why it is quiet.
fn waiting(harness: Harness, provider: &str, look: &Look, colors: &Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(10.))
        .py(px(48.))
        .child(harness_mark(harness, None, 28., look))
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(look.ink)
                .child(format!("{provider} is connected")),
        )
        .child(
            readable("conversation-waiting", WAITING_NOTICE)
                .max_w(px(420.))
                .text_center()
                .text_xs()
                .text_color(from_hex(&colors.muted))
                .child(WAITING_NOTICE),
        )
}

/// The transcript's viewport. `jump` floats over its bottom edge instead of
/// taking a row of the face, so pausing or resuming follow never resizes
/// the transcript or moves the composer.
pub(super) fn transcript_viewport(
    scroll: impl IntoElement,
    jump: Option<impl IntoElement>,
) -> gpui::Div {
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(scroll)
        .children(jump.map(|jump| {
            div()
                .absolute()
                .bottom(px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(jump)
        }))
}

pub(super) fn unavailable(placement: &str, cx: &mut Context<Desk>) -> AnyElement {
    let id = placement.to_owned();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .child(Icon::new(IconName::MessagesSquare).size(px(28.)))
        .child("The original agent session is no longer on this server.")
        .child(
            Button::new(SharedString::from(format!(
                "conversation-unavailable-{placement}"
            )))
            .label("Terminal")
            .icon(IconName::SquareTerminal)
            .ghost()
            .small()
            .on_click(
                cx.listener(move |desk, _, window, cx| desk.toggle_conversation(&id, window, cx)),
            ),
        )
        .into_any_element()
}
