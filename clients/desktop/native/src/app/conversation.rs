//! One server-owned conversation. Native controls are typed; attached TUI controls remain explicit.

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
use phux_client_core::session::agent_transcript::{
    ToolStatus, TranscriptEntry, TranscriptRole, TranscriptTool,
};
use phux_client_runtime::control::{DeliveryOutcome, Event, Status};
use phux_protocol::ResourceId;
use phux_protocol::input::key::PhysicalKey;

use phux_protocol::native_agent::{NativeDisposition, NativeIntent, NativeView};
use std::collections::HashMap;

mod questions;
use super::color::from_hex;
use super::desk::{Desk, Modal};
use super::overlays::corner;
use super::skin::Look;
use crate::connection::{Connection, ConnectionIdentity};
use crate::settings::theme::Palette;

#[derive(Clone)]
pub(crate) struct Binding {
    server: String,
    terminal: ResourceId,
    session: ResourceId,
    activation: Option<String>,
    client: std::sync::Weak<phux_client_runtime::Client>,
}

impl PartialEq for Binding {
    fn eq(&self, other: &Self) -> bool {
        // ACP activation survives daemon incarnation changes; endpoint and
        // exact parent/session/activation still identify the same owner.
        (self.server == other.server || self.activation.is_some())
            && self.terminal == other.terminal
            && self.session == other.session
            && self.activation == other.activation
            && self.client.ptr_eq(&other.client)
    }
}

impl Eq for Binding {}

struct WriteIdentity<'a> {
    connection: ConnectionIdentity,
    stream: AgentStreamInfo,
    state: AgentSessionStatus,
    native: Option<&'a NativeView>,
}

pub(crate) struct Face {
    binding: Binding,
    orphaned: bool,
    pub visible: bool,
    pub(super) input: Entity<TextareaState>,
    _subscription: Subscription,
    scroll: ScrollHandle,
    follow: bool,
    revision: u64,
    pending: Option<(u64, String)>,
    draft_changed_after_submit: bool,
    error: Option<String>,
    delivered: Option<String>,
    notice: Option<String>,
    native_commands: HashMap<u64, Option<String>>,
    interrupted_turn: Option<String>,
    question_status: HashMap<String, String>,
    questions: HashMap<String, questions::Draft>,
}

impl Face {
    pub(super) fn retain_for_placement(
        &mut self,
        live: bool,
        connection: Option<&Connection>,
    ) -> bool {
        self.orphaned = !live;
        if live {
            return true;
        }
        let (Some(activation), Some(connection)) = (&self.binding.activation, connection) else {
            return false;
        };
        if !std::ptr::eq(
            self.binding.client.as_ptr(),
            std::sync::Arc::as_ptr(&connection.client),
        ) {
            return false;
        }
        if let Some(current) = connection.conversation(&self.binding.terminal)
            && current.epoch == connection.connection_epoch()
        {
            return current.resource == self.binding.session
                && !current.closed
                && current
                    .native
                    .as_ref()
                    .is_none_or(|view| &view.activation == activation);
        }
        connection.status != Status::Attached
            || connection.topology.as_ref().is_none_or(|topology| {
                topology.agent_sessions.iter().any(|session| {
                    session.id == self.binding.session
                        && session.parent.as_ref() == Some(&self.binding.terminal)
                })
            })
    }
}

fn subscribe_input(
    input: &Entity<TextareaState>,
    placement: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> Subscription {
    let id = placement.to_owned();
    cx.subscribe_in(input, window, move |desk, _, event: &InputEvent, _, cx| {
        if let InputEvent::PressEnter { shift: false, .. } = event {
            desk.send_conversation(&id, cx);
        }
        if matches!(event, InputEvent::Change) {
            if let Some(face) = desk.conversation_faces.get_mut(&id)
                && (face.pending.is_some() || face.delivered.is_some())
            {
                face.draft_changed_after_submit = true;
                face.delivered = None;
            }
            cx.notify();
        }
    })
}

impl Desk {
    pub(crate) fn remember_native_focus(&mut self) {
        let Some(face) = self
            .focused_placement()
            .and_then(|placement| self.conversation_faces.get(&placement.id))
            .filter(|face| face.visible)
        else {
            return;
        };
        let Some(activation) = face.binding.activation.as_ref() else {
            return;
        };
        let Some(connection) = self.link.as_ref().map(|link| &link.connection) else {
            return;
        };
        let Some(server) = connection.server_id() else {
            return;
        };
        if server == face.binding.server
            || !std::ptr::eq(
                face.binding.client.as_ptr(),
                std::sync::Arc::as_ptr(&connection.client),
            )
        {
            return;
        }
        let (terminal, agent, activation) = (
            face.binding.terminal.clone(),
            face.binding.session.clone(),
            activation.clone(),
        );
        self.resume_native_conversation(terminal, agent, activation);
    }

    pub(crate) fn conversation_visible(&self, placement: &str) -> bool {
        if let Some(face) = self.conversation_faces.get(placement) {
            return face.visible;
        }
        if !self
            .conversation_faces
            .values()
            .any(|face| face.orphaned && face.visible)
        {
            return false;
        }
        self.conversation_binding(placement).is_some_and(|binding| {
            self.conversation_faces
                .values()
                .any(|face| face.orphaned && face.visible && face.binding == binding)
        })
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
            activation: connection
                .conversation(terminal)?
                .native
                .as_ref()
                .map(|view| view.activation.clone()),
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
            questions::prepare(self, placement, window, cx);
            return;
        }
        // A daemon reexec recreates layout placements, not the ACP owner.
        // Move the original editor, receipts, and question drafts together.
        if let Some((_, mut face)) = self
            .conversation_faces
            .extract_if(|_, face| face.orphaned && face.binding == binding)
            .next()
        {
            face.binding = binding;
            face.orphaned = false;
            face._subscription = subscribe_input(&face.input, placement, window, cx);
            self.conversation_faces.insert(placement.to_owned(), face);
            questions::prepare(self, placement, window, cx);
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
        let subscription = subscribe_input(&input, placement, window, cx);
        self.conversation_faces.insert(
            placement.to_owned(),
            Face {
                binding,
                orphaned: false,
                visible,
                input,
                _subscription: subscription,
                scroll: ScrollHandle::new(),
                follow: true,
                revision: 0,
                pending: None,
                draft_changed_after_submit: false,
                error: None,
                delivered: None,
                notice: None,
                native_commands: HashMap::new(),
                interrupted_turn: None,
                question_status: HashMap::new(),
                questions: HashMap::new(),
            },
        );
        questions::prepare(self, placement, window, cx);
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
        let view = self
            .link
            .as_ref()
            .and_then(|link| link.connection.conversations.get(&face.binding.session))
            .filter(|conversation| {
                !conversation.closed && conversation.state.status != AgentSessionStatus::Ended
            })
            .and_then(|conversation| conversation.native.as_ref());
        if questions::focus(face, view, window, cx) {
            return true;
        }
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
            if matches!(event, Event::StatusChanged(status) if *status != Status::Attached) {
                for face in self
                    .conversation_faces
                    .values_mut()
                    .filter(|face| face.binding.activation.is_some())
                {
                    if !face.native_commands.is_empty() {
                        let unknown = receipt_message(
                            NativeDisposition::Unknown,
                            Some("Connection changed before receipt."),
                        );
                        for question in face.native_commands.values().flatten() {
                            if let Some(status) = face.question_status.get_mut(question) {
                                status.clone_from(&unknown);
                            }
                        }
                        face.notice = Some(unknown);
                        changed = true;
                    }
                }
            }
            if let Event::NativeAgentResult {
                agent_session_id,
                operation_id,
                disposition,
                detail,
            } = event
            {
                for face in self.conversation_faces.values_mut() {
                    if &face.binding.session != agent_session_id
                        || current_client
                            .as_ref()
                            .is_none_or(|client| !client.ptr_eq(&face.binding.client))
                    {
                        continue;
                    }
                    let Some(question) = face.native_commands.get(operation_id) else {
                        continue;
                    };
                    changed = true;
                    let message = receipt_message(*disposition, detail.as_deref());
                    if question.is_none()
                        && !face
                            .pending
                            .as_ref()
                            .is_some_and(|(id, _)| id == operation_id)
                        && *disposition == NativeDisposition::Refused
                    {
                        face.interrupted_turn = None;
                    }
                    if let Some(question) = question {
                        // Do not optimistically close native UI: queued is not provider consumption.
                        if matches!(disposition, NativeDisposition::Refused) {
                            face.question_status.remove(question);
                        } else if let Some(status) = face.question_status.get_mut(question) {
                            status.clone_from(&message);
                        }
                    }
                    resolve_prompt_receipt(
                        &mut face.pending,
                        &mut face.delivered,
                        *operation_id,
                        *disposition,
                    );
                    if face.draft_changed_after_submit {
                        face.delivered = None;
                    }
                    if matches!(
                        disposition,
                        NativeDisposition::Admitted
                            | NativeDisposition::Completed
                            | NativeDisposition::Refused
                    ) {
                        face.native_commands.remove(operation_id);
                    }
                    face.notice = Some(message);
                }
                continue;
            }
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
                if face.binding.activation.is_some() {
                    continue;
                }
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
                        if !face.draft_changed_after_submit {
                            face.delivered = Some(submitted);
                        }
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
    ) -> Result<WriteIdentity<'_>, String> {
        let connection = &self.link.as_ref().ok_or("Not connected")?.connection;
        let identity = connection.identity().ok_or("Not connected")?;
        let conversation = connection
            .conversation(&binding.terminal)
            .ok_or("The agent session is no longer available")?;
        if (binding.activation.is_none() && binding.server != identity.server_id)
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
        let native = conversation.native.as_ref();
        if binding.activation.as_deref() != native.map(|view| view.activation.as_str()) {
            return Err("The ACP activation changed. Nothing was sent.".into());
        }
        if let Some(native) = native {
            if !native.connected {
                return Err(
                    "ACP controller disconnected. Controls remain disabled; no terminal fallback."
                        .into(),
                );
            }
            if stop && (!native.capabilities.interrupt.is_supported() || native.turn_id.is_none()) {
                return Err("This provider has no interruptible turn.".into());
            }
        } else if self.native_owned_terminal(&binding.terminal) {
            return Err(
                "An ACP owner is retained for this terminal. Wait for its native controls; no terminal fallback."
                    .into(),
            );
        } else if conversation.state.status == AgentSessionStatus::Blocked {
            return Err(
                "Attached-mode agent needs you. Open Terminal to answer its request.".into(),
            );
        }
        if identity.connection_epoch != conversation.epoch
            || connection.status != Status::Attached
            || (native.is_none() && !connection.ready(&binding.terminal))
        {
            return Err(
                "Waiting for the same terminal and agent session to reconnect. Nothing was sent."
                    .into(),
            );
        }
        if stop && native.is_none() && conversation.state.status != AgentSessionStatus::Working {
            return Err("Stop is available only while the agent is working.".into());
        }
        if stop && native.is_none() && conversation.stop_requested {
            return Err("Stop already requested for this working turn.".into());
        }
        Ok(WriteIdentity {
            connection: identity,
            stream: conversation.stream,
            state: conversation.state.status,
            native,
        })
    }

    pub(crate) fn conversation_writable(&self, placement: &str, stop: bool) -> bool {
        self.conversation_faces.get(placement).is_some_and(|face| {
            (face.pending.is_none() || (stop && face.binding.activation.is_some()))
                && (!stop
                    || self
                        .link
                        .as_ref()
                        .and_then(|link| link.connection.conversations.get(&face.binding.session))
                        .and_then(|conversation| conversation.native.as_ref())
                        .is_none_or(|view| face.interrupted_turn.as_ref() != view.turn_id.as_ref()))
                && (stop
                    || self
                        .link
                        .as_ref()
                        .and_then(|link| link.connection.conversations.get(&face.binding.session))
                        .is_none_or(|conversation| {
                            conversation.native.as_ref().is_none_or(|view| {
                                prompt_available(view, conversation.state.status)
                            })
                        }))
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
                    if let Some(native) = identity.native {
                        if !prompt_available(native, identity.state) {
                            return Err("This ACP agent does not accept a prompt in its current turn. Draft preserved.".into());
                        }
                        control.native_agent_command(&binding.session, &native.activation, NativeIntent::Prompt { text: text.clone() })
                            .ok_or_else(|| "Native prompt was not scheduled. Draft preserved.".to_owned())
                    } else {
                        Ok(control.apply_line(&binding.terminal, &text))
                    }
                });
                client.wake();
                result
            });
        if let Some(face) = self.conversation_faces.get_mut(placement) {
            match result {
                Ok(delivery) => {
                    face.pending = Some((delivery, text));
                    face.draft_changed_after_submit = false;
                    if binding.activation.is_some() {
                        face.native_commands.insert(delivery, None);
                        face.notice = Some("Submitting to the ACP controller…".into());
                    }
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
                    if let Some(native) = identity.native {
                        let turn_id = native.turn_id.clone().ok_or("The working turn changed")?;
                        return control
                            .native_agent_command(
                                &binding.session,
                                &native.activation,
                                NativeIntent::Interrupt {
                                    turn_id: turn_id.clone(),
                                },
                            )
                            .map(|operation| Some((operation, turn_id)))
                            .ok_or_else(|| "Native interrupt was not scheduled.".to_owned());
                    }
                    if control.send_key(
                        &binding.terminal,
                        phux_client_core::keys::named(PhysicalKey::Escape),
                    ) {
                        Ok(None)
                    } else {
                        Err("The terminal refused Stop. Nothing was sent.".into())
                    }
                });
                client.wake();
                result
            });
        if result.is_ok()
            && binding.activation.is_none()
            && let Some(conversation) = self
                .link
                .as_mut()
                .and_then(|link| link.connection.conversations.get_mut(&binding.session))
        {
            conversation.stop_requested = true;
        }
        if let Some(face) = self.conversation_faces.get_mut(placement) {
            face.error = result.as_ref().err().cloned();
            if let Ok(Some((operation, turn))) = result.as_ref() {
                face.native_commands.insert(*operation, None);
                face.interrupted_turn = Some(turn.clone());
                face.notice = Some("Interrupt submitted; waiting for ACP receipt.".into());
            }
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

fn receipt_message(disposition: NativeDisposition, detail: Option<&str>) -> String {
    let stage = match disposition {
        NativeDisposition::Admitted => "Admitted by provider",
        NativeDisposition::Queued => "Queued — not yet confirmed consumed by provider",
        NativeDisposition::Completed => "Completed",
        NativeDisposition::Refused => "Refused — nothing retried; draft preserved",
        NativeDisposition::Unknown => {
            "Outcome unknown — inspect the conversation before resending; never retried automatically"
        }
    };
    match detail.filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("{stage}: {detail}"),
        None => stage.to_owned(),
    }
}

fn prompt_available(view: &NativeView, state: AgentSessionStatus) -> bool {
    view.connected
        && view.capabilities.prompt.is_supported()
        && view.questions.is_empty()
        && (state != AgentSessionStatus::Working || view.capabilities.follow_up.is_supported())
}

fn resolve_prompt_receipt(
    pending: &mut Option<(u64, String)>,
    delivered: &mut Option<String>,
    operation: u64,
    disposition: NativeDisposition,
) {
    if matches!(
        disposition,
        NativeDisposition::Queued | NativeDisposition::Unknown
    ) {
        return;
    }
    if pending.as_ref().is_some_and(|(id, _)| *id == operation)
        && let Some((_, text)) = pending.take()
        && matches!(
            disposition,
            NativeDisposition::Admitted | NativeDisposition::Completed
        )
    {
        *delivered = Some(text);
    }
}

fn gate(
    control: &phux_client_runtime::control::ControlPlane,
    binding: &Binding,
    identity: &WriteIdentity<'_>,
    stop: bool,
) -> Result<(), String> {
    let server = control
        .server()
        .map(phux_client_ffi::projection::status::server_id);
    if server.as_deref() != Some(identity.connection.server_id.as_str())
        || control.connection_epoch() != identity.connection.connection_epoch
        || control.status() != Status::Attached
        || (identity.native.is_none() && !control.input_ready(&binding.terminal))
    {
        return Err("The connection or terminal changed. Nothing was sent.".into());
    }
    if !control.topology().is_some_and(|topology| {
        topology.agent_sessions.iter().any(|session| {
            session.id == binding.session && session.parent.as_ref() == Some(&binding.terminal)
        })
    }) {
        return Err("The agent session changed. Nothing was sent; wait for current records or open Terminal.".into());
    }
    if identity.native.is_some() {
        let current = control
            .engine()
            .and_then(|engine| engine.native_agent_view(&binding.session));
        if current.as_ref() != identity.native
            || !current.as_ref().is_some_and(|view| view.connected)
        {
            return Err("ACP state changed. Nothing was sent; wait for current controls.".into());
        }
        return Ok(());
    }
    if crate::input::native_owned(control, &binding.terminal) {
        return Err("The terminal acquired an ACP owner. No raw input was sent.".into());
    }
    // Attached-mode lifecycle has no native activation/turn/question fencing.
    if control
        .engine()
        .and_then(|engine| engine.agent_stream_info(&binding.session))
        != Some(identity.stream)
    {
        return Err(
            "The attached agent changed. Wait for current records or open Terminal.".into(),
        );
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
        .child(div().id(SharedString::from(format!("conversation-scroll-{placement}"))).flex_1().min_h_0().overflow_y_scroll()
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
                .children(question_rows)))
        .when(!following, |face| face.child(Button::new(SharedString::from(format!("conversation-live-{placement}"))).label("Jump to latest").ghost().small()
            .on_click(cx.listener(move |desk, _, _, cx| {
                if let Some(face) = desk.conversation_faces.get_mut(&live_id) {
                    face.follow = true;
                    face.scroll.set_offset(point(px(0.), px(-1_000_000.)));
                }
                cx.notify();
            }))))
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

const OMITTED_NOTICE: &str = "Earlier entries omitted from the bounded native transcript.";
const WAITING_NOTICE: &str = "Waiting for conversation entries. Transcript sharing may be disabled; lifecycle events remain visible here.";

/// A container screen readers announce as static text. GPUI only reports
/// nodes that carry both an id and a role, and plain string children carry
/// neither, so visible text that matters is mirrored into a `Label` value.
fn readable(
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    div().id(id.into()).role(gpui::Role::Label).aria_value(text)
}

/// The transcript as one named list whose children are the entry rows.
fn conversation_list(
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

fn speaker(role: TranscriptRole) -> &'static str {
    match role {
        TranscriptRole::User => "You",
        TranscriptRole::Assistant => "Assistant",
        TranscriptRole::Thinking => "Thinking",
        TranscriptRole::Tool => "Tool",
        TranscriptRole::System => "Activity",
    }
}

fn tool_status(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Running => "running",
        ToolStatus::Ok => "ok",
        ToolStatus::Error => "error",
    }
}

/// The first non-empty line of `text`, trimmed.
fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

/// What a screen reader announces when it lands on a row: the speaker (or
/// tool, state and command) plus the first line. The full body, summary and
/// output are separate `Label` children, so this stays a short name.
fn entry_label(entry: &TranscriptEntry) -> String {
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
fn readable_lines(id: &'static str, text: &str) -> gpui::Stateful<gpui::Div> {
    readable(id, text.to_owned())
        .text_sm()
        .children(text.lines().map(|line| div().child(line.to_owned())))
}

/// The entry body: markdown prose for the assistant and thinking, plain lines
/// otherwise. Screen readers get the full text either way.
fn entry_body(entry: &TranscriptEntry) -> gpui::Stateful<gpui::Div> {
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
fn tool_details(
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
fn transcript_row(
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

#[cfg(test)]
mod tests {
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
    pub(crate) fn a11y(
        element: &impl gpui::Element,
    ) -> (Option<gpui::Role>, gpui::accesskit::Node) {
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
}
