//! The conversation face: header, transcript, composer and controls.

use super::*;

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
    let state = if conversation.closed {
        "closed · read-only"
    } else if conversation
        .native
        .as_ref()
        .is_some_and(|view| !view.connected)
    {
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
    let title = format!(
        "{} · {} · {state}",
        conversation.state.provider.as_deref().unwrap_or("Agent"),
        if conversation.native.is_some() {
            "ACP"
        } else {
            "Attached TUI"
        },
    );
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
    let rows = conversation
        .entries
        .iter()
        .map(|entry| transcript_row(entry, colors, &look, &mono))
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
        conversation.native.as_ref().filter(|_| {
            !conversation.closed && conversation.state.status != AgentSessionStatus::Ended
        }),
        !conversation.closed
            && desk.link.as_ref().is_some_and(|link| {
                link.connection.status == Status::Attached
                    && conversation.epoch == link.connection.connection_epoch()
            }),
        placement,
        &look,
        cx,
    );
    let native = conversation.native.is_some();
    // Native drafts are local: a provider question or delivery fence blocks Send,
    // not composing the next message.
    let editable = if native {
        !conversation.closed && conversation.state.status != AgentSessionStatus::Ended
    } else {
        !pending && writable
    };
    let following = face.follow;
    let terminal_id = placement.to_owned();
    let send_id = placement.to_owned();
    let stop_id = placement.to_owned();
    let live_id = placement.to_owned();
    let scroll_id = placement.to_owned();
    let focus_id = placement.to_owned();
    div().size_full().flex().flex_col().gap(px(10.)).text_color(from_hex(&colors.theme.foreground))
        .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |desk, _, window, cx| {
            if desk.focused_placement().map(|placement| placement.id.as_str()) != Some(focus_id.as_str())
                && let Some(tab) = desk.active_tab_id()
            {
                desk.focus_placement(&tab, &focus_id, window, cx);
            }
        }))
        .child(div().flex().items_center().justify_between().gap(px(8.)).text_sm()
            .child(title)
            .child(Button::new(SharedString::from(format!("conversation-terminal-{placement}"))).label("Terminal").ghost().small()
                .on_click(cx.listener(move |desk, _, window, cx| desk.toggle_conversation(&terminal_id, window, cx)))))
        .children(native_identity.map(|identity| div().text_xs().text_color(from_hex(&colors.muted)).child(identity)))
        .child(transcript_viewport(div().id(SharedString::from(format!("conversation-scroll-{placement}"))).flex_1().min_h_0().overflow_y_scroll()
            .track_scroll(&scroll).on_scroll_wheel(cx.listener(move |desk, _, _, cx| {
                if let Some(face) = desk.conversation_faces.get_mut(&scroll_id) {
                    // Explicit reader interaction pauses follow until Jump to latest.
                    face.follow = false;
                    cx.notify();
                }
            }))
            // Only transcript rows live in the List; notices and question
            // panels sit beside it so every List child is a ListItem.
            .child(div().flex().flex_col().gap(px(12.)).pb(px(12.))
                .when(omitted, |list| list.child(readable("conversation-omitted", OMITTED_NOTICE).text_xs().text_color(from_hex(&colors.muted)).child(OMITTED_NOTICE)))
                .when(!has_rows, |list| list.child(readable("conversation-waiting", WAITING_NOTICE).child(WAITING_NOTICE)))
                .when(has_rows, |list| list.child(conversation_list(rows)))
                .children(question_rows)),
            // Floats over the transcript: pausing follow must not resize it.
            (!following).then(|| Button::new(SharedString::from(format!("conversation-live-{placement}"))).label("Jump to latest").small()
            .on_click(cx.listener(move |desk, _, _, cx| {
                if let Some(face) = desk.conversation_faces.get_mut(&live_id) {
                    face.follow = true;
                    face.scroll.set_offset(point(px(0.), px(-1_000_000.)));
                }
                cx.notify();
            })))))
        .children(error.map(|error| div().text_sm().text_color(from_hex(&colors.theme.warning)).child(error)))
        .children(notice.map(|notice| div().text_xs().text_color(from_hex(&colors.muted)).child(notice)))
        .child(Textarea::new(&input).aria_label("Message").w_full().disabled(!editable))
        .child(div().flex().items_center().gap(px(8.))
            .child(Button::new(SharedString::from(format!("conversation-send-{placement}"))).label(if pending { "Sending…" } else { "Send" }).primary().small().disabled(!writable || empty)
                .on_click(cx.listener(move |desk, _, _, cx| desk.send_conversation(&send_id, cx))))
            .child(Button::new(SharedString::from(format!("conversation-stop-{placement}"))).label("Stop · Esc").accessibility_label("Stop agent").ghost().small().disabled(!stopping)
                .on_click(cx.listener(move |desk, _, _, cx| desk.stop_conversation(&stop_id, cx))))
            .child(div().text_xs().text_color(from_hex(&colors.muted)).child(if native && blocked {
                "Respond to the provider request above · first response across viewers wins"
            } else if blocked {
                "Attached TUI needs you · Open Terminal to answer approvals or permissions"
            } else if writable {
                "Enter sends · Shift+Enter adds a line"
            } else {
                "Read-only until this exact terminal and agent session are writable"
            })))
        .into_any_element()
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
        .gap(px(12.))
        .child("The original agent session is no longer on this server.")
        .child(
            Button::new(SharedString::from(format!(
                "conversation-unavailable-{placement}"
            )))
            .label("Terminal")
            .ghost()
            .small()
            .on_click(
                cx.listener(move |desk, _, window, cx| desk.toggle_conversation(&id, window, cx)),
            ),
        )
        .into_any_element()
}
