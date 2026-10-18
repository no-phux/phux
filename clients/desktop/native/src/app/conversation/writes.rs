//! Writing to a conversation: the identity a write must still match, and
//! Send, Stop and Escape, each fenced so nothing reaches a replaced owner.

use super::*;

impl Desk {
    pub(super) fn conversation_write_identity(
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
        if !self.conversation_has_keys(&id) {
            return false;
        }
        if self.conversation_writable(&id, true) {
            self.stop_conversation(&id, cx);
        }
        // Escape must never leak into an idle terminal underneath the native face.
        true
    }
}

pub(super) fn prompt_available(view: &NativeView, state: AgentSessionStatus) -> bool {
    view.connected
        && view.capabilities.prompt.is_supported()
        && view.questions.is_empty()
        && (state != AgentSessionStatus::Working || view.capabilities.follow_up.is_supported())
}

pub(super) fn gate(
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
