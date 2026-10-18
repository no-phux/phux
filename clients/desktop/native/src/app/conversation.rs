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
mod receipts;
mod render;
#[cfg(test)]
mod tests;
mod transcript;
mod writes;

pub(crate) use render::render;
use transcript::{OMITTED_NOTICE, WAITING_NOTICE, conversation_list, readable};
use writes::gate;

use super::color::from_hex;
use super::desk::{Desk, Modal};
use super::notifications::Kind;
use super::overlays::corner;
use super::pane_identity::Harness;
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
    /// Thinking and tool rows a reader opened or folded, by entry id.
    folds: HashMap<String, bool>,
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

    /// A pane's own face decides; an orphaned face it inherits next; a pane
    /// with neither follows the window's native-agents preference.
    pub(crate) fn conversation_visible(&self, placement: &str) -> bool {
        if let Some(face) = self.conversation_faces.get(placement) {
            return face.visible;
        }
        let orphans = self.conversation_faces.values().any(|face| face.orphaned);
        if !orphans && !self.prefs.native_agents {
            return false;
        }
        let Some(binding) = self.conversation_binding(placement) else {
            return false;
        };
        self.conversation_faces
            .values()
            .find(|face| face.orphaned && face.binding == binding)
            .map_or(self.prefs.native_agents, |face| face.visible)
    }

    /// Whether `placement`'s composer owns the keyboard: its face shows, and
    /// the conversation it binds is still on this connection. A face whose
    /// session is gone renders `unavailable` and must not swallow keys.
    pub(crate) fn conversation_has_keys(&self, placement: &str) -> bool {
        if !self.conversation_visible(placement) {
            return false;
        }
        let Some(face) = self.conversation_faces.get(placement) else {
            // Shown without a face means a live binding was just resolved.
            return true;
        };
        self.link.as_ref().is_some_and(|link| {
            link.connection
                .conversations
                .contains_key(&face.binding.session)
        })
    }

    /// Whether the window has anything for the native-agents toggle to
    /// switch: a live conversation on this server, or the toggle already on.
    pub(crate) fn native_agents_offered(&self) -> bool {
        self.prefs.native_agents
            || self.link.as_ref().is_some_and(|link| {
                link.connection
                    .conversations
                    .values()
                    .any(|conversation| !conversation.closed)
            })
    }

    /// Snap every agent pane in this window to its conversation, or every
    /// one back to its terminal. Panes whose agent starts later follow.
    pub(crate) fn toggle_native_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_prefs(|prefs| prefs.native_agents = !prefs.native_agents, cx);
        let focused = self
            .focused_placement()
            .map(|placement| placement.id.clone());
        if let (Some(tab), Some(placement)) = (self.active_tab_id(), focused) {
            self.focus_placement(&tab, &placement, window, cx);
        }
        cx.notify();
    }

    /// Every face this window holds shows its conversation, or its terminal.
    pub(crate) fn snap_conversations(&mut self, native: bool) {
        for face in self.conversation_faces.values_mut() {
            face.visible = native;
        }
    }

    /// The way to a native view for an agent pane that has none: copy the
    /// harness's phux plugin install, or open New Agent for one with no
    /// plugin. Nothing is installed from here.
    pub(crate) fn offer_native_setup(
        &mut self,
        harness: Harness,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(command) = harness.plugin_install() else {
            self.execute("native-agent", window, cx);
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(command.to_owned()));
        self.post(
            Kind::Info,
            &format!("{} conversation setup copied", harness.label()),
            Some(format!(
                "Run `{command}`, then restart {} in this pane. Its conversation appears here.",
                harness.label()
            )),
        );
        cx.notify();
    }

    /// The focused pane's conversation, or its terminal.
    pub(crate) fn toggle_focused_conversation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(placement) = self
            .focused_placement()
            .map(|placement| placement.id.clone())
            .filter(|placement| self.conversation_eligible(placement))
        {
            self.toggle_conversation(&placement, window, cx);
        }
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
        let conversation = connection.conversation(terminal)?;
        Some(Binding {
            server: connection.server_id()?,
            terminal: terminal.clone(),
            session: conversation.resource.clone(),
            activation: conversation
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
                folds: HashMap::new(),
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
        if !self.conversation_faces.contains_key(placement) && self.conversation_visible(placement)
        {
            self.prepare_conversation(placement, window, cx);
        }
        if !self.conversation_has_keys(placement) {
            return false;
        }
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
}
