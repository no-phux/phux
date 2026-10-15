//! Per-placement native transcript and composer. Input still goes to the one PTY.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, ScrollHandle, SharedString, Subscription, Window, div, point, px,
};
use gpui_kit::base::{Disableable, ScrollbarHandle};
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use phux_client_core::session::AgentStreamInfo;
use phux_client_core::session::agent_stream::AgentSessionStatus;
use phux_client_core::session::agent_transcript::{ToolStatus, TranscriptEntry, TranscriptRole};
use phux_client_runtime::control::{DeliveryOutcome, Event, Status};
use phux_protocol::ResourceId;
use phux_protocol::input::key::PhysicalKey;

use super::color::from_hex;
use super::desk::{Desk, Modal};
use crate::connection::ConnectionIdentity;
use crate::settings::theme::Palette;

#[derive(Clone)]
pub(crate) struct Binding {
    server: String,
    terminal: ResourceId,
    session: ResourceId,
    client: std::sync::Weak<phux_client_runtime::Client>,
}

impl PartialEq for Binding {
    fn eq(&self, other: &Self) -> bool {
        self.server == other.server
            && self.terminal == other.terminal
            && self.session == other.session
            && self.client.ptr_eq(&other.client)
    }
}

impl Eq for Binding {}

struct WriteIdentity {
    connection: ConnectionIdentity,
    stream: AgentStreamInfo,
    state: AgentSessionStatus,
}

pub(crate) struct Face {
    binding: Binding,
    pub visible: bool,
    pub(super) input: Entity<TextareaState>,
    _subscription: Subscription,
    scroll: ScrollHandle,
    follow: bool,
    revision: u64,
    pending: Option<(u64, String)>,
    error: Option<String>,
    delivered: Option<String>,
}

impl Desk {
    pub(crate) fn conversation_visible(&self, placement: &str) -> bool {
        self.conversation_faces
            .get(placement)
            .is_some_and(|face| face.visible)
    }

    pub(crate) fn conversation_eligible(&self, placement: &str) -> bool {
        self.placement(placement)
            .and_then(|placement| placement.terminal_id.as_ref())
            .and_then(|terminal| self.link.as_ref()?.connection.conversation(terminal))
            .is_some()
    }

    fn conversation_binding(&self, placement: &str) -> Option<Binding> {
        let terminal = self.placement(placement)?.terminal_id.as_ref()?;
        let connection = &self.link.as_ref()?.connection;
        Some(Binding {
            server: connection.server_id()?,
            terminal: terminal.clone(),
            session: connection.conversation(terminal)?.resource.clone(),
            client: std::sync::Arc::downgrade(&connection.client),
        })
    }

    pub(crate) fn prepare_conversation(
        &mut self,
        placement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(binding) = self.conversation_binding(placement) else {
            return;
        };
        if self
            .conversation_faces
            .get(placement)
            .is_some_and(|face| face.binding == binding)
        {
            return;
        }
        // Navigation to another AgentSession creates another draft and scroll scope.
        let visible = self.conversation_visible(placement);
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message this terminal's agent")
                .auto_grow(2, 6)
                .submit_on_enter(true)
        });
        let id = placement.to_owned();
        let subscription =
            cx.subscribe_in(&input, window, move |desk, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    desk.send_conversation(&id, cx);
                }
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
        self.conversation_faces.insert(
            placement.to_owned(),
            Face {
                binding,
                visible,
                input,
                _subscription: subscription,
                scroll: ScrollHandle::new(),
                follow: true,
                revision: 0,
                pending: None,
                error: None,
                delivered: None,
            },
        );
    }

    pub fn toggle_conversation(
        &mut self,
        placement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prepare_conversation(placement, window, cx);
        if let Some(face) = self.conversation_faces.get_mut(placement) {
            face.visible = !face.visible;
        }
        if let Some(tab) = self.active_tab_id() {
            self.focus_placement(&tab, placement, window, cx);
        }
        cx.notify();
    }

    pub(crate) fn focus_conversation(
        &mut self,
        placement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(face) = self
            .conversation_faces
            .get(placement)
            .filter(|face| face.visible)
        else {
            return false;
        };
        face.input
            .clone()
            .update(cx, |input, cx| input.focus(window, cx));
        true
    }

    /// Receipts resolve even while the terminal face or another tab is showing.
    pub(crate) fn conversation_receipts(&mut self, events: &[Event]) -> bool {
        let current_client = self
            .client()
            .map(|client| std::sync::Arc::downgrade(&client));
        let mut changed = false;
        for event in events {
            let Event::InputDelivery {
                delivery_id,
                outcome,
                message,
                ..
            } = event
            else {
                continue;
            };
            for face in self.conversation_faces.values_mut() {
                if current_client
                    .as_ref()
                    .is_none_or(|client| !client.ptr_eq(&face.binding.client))
                {
                    continue;
                }
                // A terminal-face paste can fence this same runtime even when
                // it was not submitted by a native composer.
                changed |= *outcome == DeliveryOutcome::Unknown && face.visible;
                if face
                    .pending
                    .as_ref()
                    .is_none_or(|(id, _)| id != delivery_id)
                {
                    continue;
                }
                let Some((_, submitted)) = face.pending.take() else {
                    continue;
                };
                changed = true;
                match outcome {
                    DeliveryOutcome::Delivered => {
                        face.delivered = Some(submitted);
                        face.error = None;
                    }
                    DeliveryOutcome::Refused => {
                        face.error = Some(if message.is_empty() {
                            "Message was refused; draft preserved.".into()
                        } else {
                            message.clone()
                        });
                    }
                    DeliveryOutcome::Unknown => {
                        face.error = Some("Delivery is unknown. Read the terminal before resending; the draft is preserved and is never retried automatically.".into());
                    }
                }
            }
        }
        changed
    }

    fn conversation_write_identity(
        &self,
        binding: &Binding,
        stop: bool,
    ) -> Result<WriteIdentity, String> {
        let connection = &self.link.as_ref().ok_or("Not connected")?.connection;
        let identity = connection.identity().ok_or("Not connected")?;
        let conversation = connection
            .conversation(&binding.terminal)
            .ok_or("The agent session is no longer available")?;
        if binding.server != identity.server_id
            || binding.session != conversation.resource
            || !binding
                .client
                .ptr_eq(&std::sync::Arc::downgrade(&connection.client))
        {
            return Err("This agent session was replaced. Nothing was sent.".into());
        }
        if conversation.closed || conversation.state.status == AgentSessionStatus::Ended {
            return Err("This agent session has ended. Its conversation is read-only.".into());
        }
        if conversation.state.status == AgentSessionStatus::Blocked {
            return Err("Agent needs you. Open Terminal to answer its approval or permission request; the draft is preserved.".into());
        }
        if identity.connection_epoch != conversation.epoch || !connection.ready(&binding.terminal) {
            return Err(
                "Waiting for the same terminal and agent session to reconnect. Nothing was sent."
                    .into(),
            );
        }
        if stop && conversation.state.status != AgentSessionStatus::Working {
            return Err("Stop is available only while the agent is working.".into());
        }
        if stop && conversation.stop_requested {
            return Err("Stop already requested for this working turn.".into());
        }
        Ok(WriteIdentity {
            connection: identity,
            stream: conversation.stream,
            state: conversation.state.status,
        })
    }

    pub(crate) fn conversation_writable(&self, placement: &str, stop: bool) -> bool {
        self.conversation_faces.get(placement).is_some_and(|face| {
            face.pending.is_none()
                && self
                    .conversation_write_identity(&face.binding, stop)
                    .is_ok()
        })
    }

    pub(crate) fn send_conversation(&mut self, placement: &str, cx: &mut Context<Self>) {
        let Some(face) = self.conversation_faces.get(placement) else {
            return;
        };
        if face.pending.is_some() {
            return;
        }
        let text = face.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let binding = face.binding.clone();
        let result = self
            .conversation_write_identity(&binding, false)
            .and_then(|identity| {
                let client = self.client().ok_or("Not connected")?;
                let result = client.with_control(|control| {
                    gate(control, &binding, &identity, false)?;
                    Ok(control.apply_line(&binding.terminal, &text))
                });
                client.wake();
                result
            });
        if let Some(face) = self.conversation_faces.get_mut(placement) {
            match result {
                Ok(delivery) => {
                    face.pending = Some((delivery, text));
                    face.error = None;
                }
                Err(message) => face.error = Some(message),
            }
        }
        cx.notify();
    }

    pub(crate) fn stop_conversation(&mut self, placement: &str, cx: &mut Context<Self>) {
        if !self.conversation_writable(placement, true) {
            return;
        }
        let Some(face) = self.conversation_faces.get(placement) else {
            return;
        };
        let binding = face.binding.clone();
        let result = self
            .conversation_write_identity(&binding, true)
            .and_then(|identity| {
                let client = self.client().ok_or("Not connected")?;
                let result = client.with_control(|control| {
                    gate(control, &binding, &identity, true)?;
                    if control.send_key(
                        &binding.terminal,
                        phux_client_core::keys::named(PhysicalKey::Escape),
                    ) {
                        Ok(())
                    } else {
                        Err("The terminal refused Stop. Nothing was sent.".into())
                    }
                });
                client.wake();
                result
            });
        if result.is_ok()
            && let Some(conversation) = self
                .link
                .as_mut()
                .and_then(|link| link.connection.conversations.get_mut(&binding.session))
        {
            conversation.stop_requested = true;
        }
        if let Some(face) = self.conversation_faces.get_mut(placement) {
            face.error = result.err();
        }
        cx.notify();
    }

    pub(crate) fn conversation_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !matches!(self.modal, Modal::None) || self.find.is_some() {
            return false;
        }
        let Some(id) = self
            .focused_placement()
            .map(|placement| placement.id.clone())
        else {
            return false;
        };
        if !self.conversation_visible(&id) {
            return false;
        }
        if self.conversation_writable(&id, true) {
            self.stop_conversation(&id, cx);
        }
        // Escape must never leak into an idle terminal underneath the native face.
        true
    }
}

fn gate(
    control: &phux_client_runtime::control::ControlPlane,
    binding: &Binding,
    identity: &WriteIdentity,
    stop: bool,
) -> Result<(), String> {
    let server = control
        .server()
        .map(phux_client_ffi::projection::status::server_id);
    if server.as_deref() != Some(identity.connection.server_id.as_str())
        || control.connection_epoch() != identity.connection.connection_epoch
        || control.status() != Status::Attached
        || !control.input_ready(&binding.terminal)
    {
        return Err("The connection or terminal changed. Nothing was sent.".into());
    }
    // The cursor fences unread lifecycle changes (especially permission requests).
    if control
        .engine()
        .and_then(|engine| engine.agent_stream_info(&binding.session))
        != Some(identity.stream)
        || !control.topology().is_some_and(|topology| {
            topology.agent_sessions.iter().any(|session| {
                session.id == binding.session && session.parent.as_ref() == Some(&binding.terminal)
            })
        })
    {
        return Err("The agent session changed. Nothing was sent; wait for current records or open Terminal.".into());
    }
    if identity.state == AgentSessionStatus::Blocked {
        return Err(
            "Agent needs you. Open Terminal to answer its approval or permission request.".into(),
        );
    }
    if identity.state == AgentSessionStatus::Ended
        || (stop && identity.state != AgentSessionStatus::Working)
    {
        return Err("The agent session ended or is not working. Nothing was sent.".into());
    }
    Ok(())
}

pub(crate) fn render(
    desk: &mut Desk,
    placement: &str,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    desk.prepare_conversation(placement, window, cx);
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
        "{} · {state}",
        conversation.state.provider.as_deref().unwrap_or("Agent")
    );
    let rows = conversation
        .entries
        .iter()
        .map(|entry| transcript_row(entry, colors))
        .collect::<Vec<_>>();
    let omitted = conversation.omitted;
    let input = face.input.clone();
    let empty = input.read(cx).value().trim().is_empty();
    let scroll = face.scroll.clone();
    let pending = face.pending.is_some();
    let error = face.error.clone();
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
        .child(div().id(SharedString::from(format!("conversation-scroll-{placement}"))).flex_1().min_h_0().overflow_y_scroll()
            .track_scroll(&scroll).on_scroll_wheel(cx.listener(move |desk, _, _, cx| {
                if let Some(face) = desk.conversation_faces.get_mut(&scroll_id) {
                    // Explicit reader interaction pauses follow until Jump to latest.
                    face.follow = false;
                    cx.notify();
                }
            }))
            .child(div().flex().flex_col().gap(px(12.)).pb(px(12.))
                .when(omitted, |list| list.child(div().text_xs().text_color(from_hex(&colors.muted)).child("Earlier entries omitted from the bounded native transcript.")))
                .when(rows.is_empty(), |list| list.child("Waiting for conversation entries. Transcript sharing may be disabled; lifecycle events remain visible here."))
                .children(rows)))
        .when(!following, |face| face.child(Button::new(SharedString::from(format!("conversation-live-{placement}"))).label("Jump to latest").ghost().small()
            .on_click(cx.listener(move |desk, _, _, cx| {
                if let Some(face) = desk.conversation_faces.get_mut(&live_id) {
                    face.follow = true;
                    face.scroll.set_offset(point(px(0.), px(-1_000_000.)));
                }
                cx.notify();
            }))))
        .children(error.map(|error| div().text_sm().text_color(from_hex(&colors.theme.warning)).child(error)))
        .child(Textarea::new(&input).w_full().disabled(pending))
        .child(div().flex().items_center().gap(px(8.))
            .child(Button::new(SharedString::from(format!("conversation-send-{placement}"))).label(if pending { "Sending…" } else { "Send" }).primary().small().disabled(!writable || empty)
                .on_click(cx.listener(move |desk, _, _, cx| desk.send_conversation(&send_id, cx))))
            .child(Button::new(SharedString::from(format!("conversation-stop-{placement}"))).label("Stop · Esc").ghost().small().disabled(!stopping)
                .on_click(cx.listener(move |desk, _, _, cx| desk.stop_conversation(&stop_id, cx))))
            .child(div().text_xs().text_color(from_hex(&colors.muted)).child(if blocked {
                "Agent needs you · Open Terminal to answer approvals or permissions"
            } else if writable {
                "Enter sends · Shift+Enter adds a line"
            } else {
                "Read-only until this exact terminal and agent session are writable"
            })))
        .into_any_element()
}

fn unavailable(placement: &str, cx: &mut Context<Desk>) -> AnyElement {
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

fn transcript_row(entry: &TranscriptEntry, colors: &Palette) -> AnyElement {
    let label = match entry.role {
        TranscriptRole::User => "You",
        TranscriptRole::Assistant => "Assistant",
        TranscriptRole::Thinking => "Thinking",
        TranscriptRole::Tool => "Tool",
        TranscriptRole::System => "Activity",
    };
    let mut row = div()
        .id(SharedString::from(format!(
            "conversation-entry-{}",
            entry.id
        )))
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(10.))
        .rounded(px(8.))
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
        .child(
            if matches!(
                entry.role,
                TranscriptRole::Assistant | TranscriptRole::Thinking
            ) {
                TextView::markdown(
                    SharedString::from(format!("conversation-prose-{}", entry.id)),
                    entry.text.clone(),
                )
                .selectable(true)
                .scrollable(false)
                .into_any_element()
            } else {
                div()
                    .text_sm()
                    .children(entry.text.lines().map(|line| div().child(line.to_owned())))
                    .into_any_element()
            },
        );
    if let Some(tool) = &entry.tool {
        let status = match tool.status {
            ToolStatus::Running => "running",
            ToolStatus::Ok => "ok",
            ToolStatus::Error => "error",
        };
        row = row
            .child(div().text_sm().child(format!("{} · {status}", tool.name)))
            .child(
                div()
                    .text_sm()
                    .text_color(from_hex(&colors.muted))
                    .child(tool.summary.clone()),
            );
        if !tool.output.is_empty() {
            row = row.child(
                div()
                    .text_sm()
                    .font_family("Paper Mono")
                    .children(tool.output.lines().map(|line| div().child(line.to_owned()))),
            );
        }
    }
    row.into_any_element()
}
