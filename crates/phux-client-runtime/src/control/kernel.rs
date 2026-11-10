//! Engine outcomes and kernel effect processing.

use super::{
    ControlError, ControlPlane, EngineApplyError, EngineEvent, EngineHandle, EngineOutcome, Event,
    FrameKind, InputEvent, KernelEffect, KernelSend, KernelStatus, ResourceId, ServerFeature,
};

/// One engine batch handed out by [`ControlPlane::prepare_engine_apply`].
pub(super) struct EngineApply {
    pub(super) engine: EngineHandle,
    pub(super) events: Vec<EngineEvent>,
    pub(super) geometry: Vec<(usize, ResourceId, bool)>,
}

impl ControlPlane {
    // ----- the kernel --------------------------------------------------

    /// Apply one normalized engine event on the owner thread.
    ///
    /// Bindings normally use [`ControlPlane::feed`](super::ControlPlane::feed),
    /// which performs protocol validation first. This lower-level entry point
    /// exists for native embedders that already hold typed runtime events.
    pub fn apply_engine_event(&mut self, event: EngineEvent) -> Result<(), ControlError> {
        self.apply_engine_events(vec![event])
    }

    /// Apply a pump's queued typed events in order and project each damaged
    /// terminal once after the whole batch. Every outcome is still processed
    /// in event order, including acknowledgements and resync evidence.
    pub fn apply_engine_events(&mut self, events: Vec<EngineEvent>) -> Result<(), ControlError> {
        if events.is_empty() {
            return Ok(());
        }
        let EngineApply {
            engine,
            events,
            geometry,
        } = self.prepare_engine_apply(events)?;
        let outcomes = engine.apply_batch(events);
        self.complete_engine_apply(geometry, outcomes)
    }

    /// The control-plane half before an engine batch: note what the events
    /// announce and hand back what the owner thread must apply. The caller
    /// may release the control lock for the round trip itself.
    pub(super) fn prepare_engine_apply(
        &mut self,
        events: Vec<EngineEvent>,
    ) -> Result<EngineApply, ControlError> {
        let Some(engine) = self.engine.clone() else {
            return Err(ControlError::Protocol(
                "stateful frame arrived before the session kernel was initialized".to_owned(),
            ));
        };
        for event in &events {
            self.note_engine_event(event);
        }
        let geometry = super::geometry::bootstrap_changes(&events);
        Ok(EngineApply {
            engine,
            events,
            geometry,
        })
    }

    /// The control-plane half after an engine batch.
    pub(super) fn complete_engine_apply(
        &mut self,
        geometry: Vec<(usize, ResourceId, bool)>,
        outcomes: Result<Vec<EngineOutcome>, crate::engine::EngineError>,
    ) -> Result<(), ControlError> {
        let outcomes = outcomes.map_err(|error| ControlError::Protocol(error.to_string()))?;
        self.note_geometry_bootstraps(geometry, &outcomes);
        self.process_engine_outcomes(outcomes)
    }

    fn process_engine_outcomes(
        &mut self,
        outcomes: Vec<EngineOutcome>,
    ) -> Result<(), ControlError> {
        let mut strongest_error = None;
        for outcome in outcomes {
            if let Err(error) = self.process_outcome(outcome, true) {
                strongest_error = Some(ControlError::prefer(strongest_error, error));
            }
        }
        strongest_error.map_or(Ok(()), Err)
    }

    fn note_engine_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::AttachStarted {
                attach_id,
                terminals,
            } => {
                self.active_attach_id = Some(*attach_id);
                self.attach_terminals = terminals.iter().cloned().collect();
            }
            EngineEvent::AttachReady { attach_id } if self.active_attach_id == Some(*attach_id) => {
                self.attached_once = true;
            }
            EngineEvent::Closed { terminal_id, .. } => {
                self.attach_terminals.remove(terminal_id);
            }
            EngineEvent::AgentSessionDeclared { terminal_id, .. } => {
                self.agent_delivery.admit(terminal_id);
                if self.status == super::Status::Attached {
                    self.release_agent_delivery();
                }
            }
            _ => {}
        }
    }

    pub(super) fn apply_engine(&mut self, event: EngineEvent) -> Result<(), ControlError> {
        self.apply_engine_events(vec![event])
    }

    /// Execute every declarative effect before considering the update
    /// result: a codec error can require an acknowledgement and a resync in
    /// the same outcome.
    pub(super) fn process_outcome(
        &mut self,
        outcome: EngineOutcome,
        strict: bool,
    ) -> Result<(), ControlError> {
        let resync = outcome.resync_required();
        for effect in outcome.effects {
            self.process_effect(effect);
        }
        if resync {
            return Err(ControlError::Resync);
        }
        if let Some(error) = outcome.error {
            if strict {
                return Err(match error {
                    EngineApplyError::InvalidState(message) => ControlError::InvalidState(message),
                    EngineApplyError::Protocol(message) => {
                        ControlError::Protocol(format!("session kernel: {message}"))
                    }
                });
            }
            tracing::debug!(%error, "session kernel ignored an event");
        }
        Ok(())
    }

    pub(super) fn process_effect(&mut self, effect: KernelEffect) {
        match effect {
            KernelEffect::Send(send) => self.process_send(send),
            KernelEffect::Damage(damage) => {
                if damage.kind != phux_client_core::session::KernelDamageKind::Removed {
                    self.stream_recoveries.remove(&damage.terminal_id);
                    if !self.damaged.contains(&damage.terminal_id) {
                        self.damaged.push(damage.terminal_id);
                    }
                }
            }
            KernelEffect::Status(status) => self.process_status(status),
            KernelEffect::Job(_) => {}
            KernelEffect::AgentPublication(publication) => {
                self.commit_agent_publication(publication);
            }
            KernelEffect::Document(publication) => self.push_event(Event::DocumentPublished {
                publication: std::sync::Arc::new(publication),
            }),
        }
    }

    pub(super) fn process_send(&mut self, send: KernelSend) {
        if !self.options.automatic_lifecycle {
            self.push_event(Event::KernelSend(send));
            return;
        }
        let frame = match send {
            KernelSend::Input { terminal_id, event } => {
                let Some(frame) = input_frame(terminal_id, event) else {
                    return;
                };
                frame
            }
            KernelSend::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => FrameKind::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            },
            KernelSend::HistoryRequest {
                key,
                cursor,
                max_bytes,
                max_rows,
            } => FrameKind::HistoryRequest {
                terminal_id: key.terminal_id,
                stream_id: key.stream_id,
                bootstrap_id: key.bootstrap_id,
                cursor: cursor.into(),
                max_bytes,
                max_rows,
            },
            // The kernel asks for the event stream on every attach release;
            // the handshake already subscribed, so this is a harmless
            // re-subscribe carrying the consumer's cursor.
            KernelSend::SubscribeEvents {
                terminal,
                after_seq,
            } => FrameKind::SubscribeEvents {
                terminal,
                after_seq: after_seq
                    .or(self.options.event_after_seq)
                    .filter(|_| self.server_has(ServerFeature::EventJournal)),
            },
        };
        self.queue_frame(&frame);
    }

    pub(super) fn process_status(&mut self, status: KernelStatus) {
        match status {
            KernelStatus::Engine { key, status } => match status {
                phux_client_core::engine::EngineStatus::Bell => self.push_event(Event::Bell {
                    terminal_id: key.terminal_id,
                }),
                phux_client_core::engine::EngineStatus::ClipboardWrite(text) => {
                    if self.status != super::Status::Attached
                        || self.clipboard_focus.as_ref() != Some(&key.terminal_id)
                    {
                        return;
                    }
                    self.push_event(Event::ClipboardWrite {
                        terminal_id: key.terminal_id,
                        text,
                    });
                }
                phux_client_core::engine::EngineStatus::Title(title) => {
                    if let Some(relabel) = self.relabel_in_flight(&key.terminal_id) {
                        relabel.title = Some(title.clone());
                    }
                    if let Some(pane) = self
                        .topology
                        .as_mut()
                        .and_then(|topology| topology.pane_mut(&key.terminal_id))
                    {
                        pane.title = Some(title.clone());
                    }
                    self.push_event(Event::TitleChanged {
                        terminal_id: key.terminal_id,
                        title,
                    });
                }
            },
            KernelStatus::ResyncRequired {
                terminal_id,
                reason,
                ..
            } => self.push_event(Event::ResyncRequired {
                terminal_id,
                reason,
            }),
            KernelStatus::History { key, status } => self.push_event(Event::History {
                terminal_id: key.terminal_id,
                status,
            }),
            KernelStatus::HistoryUnavailable { key, reason } => {
                self.push_event(Event::HistoryUnavailable {
                    terminal_id: key.terminal_id,
                    reason,
                });
            }
            KernelStatus::Exited {
                terminal_id,
                exit_status,
                signal,
                reason,
            } => {
                // An exited shell runs nothing; a retained resource is not busy.
                self.command_states.insert(terminal_id.clone(), false);
                self.push_event(Event::Exited {
                    terminal_id,
                    exit_status,
                    signal,
                    reason,
                });
            }
            KernelStatus::Unsupported {
                terminal_id,
                codec_tag,
            } => tracing::warn!(
                %terminal_id,
                codec_tag,
                "the server streamed a resource in a codec this build cannot decode; ignoring it"
            ),
            // Folded from the agent event itself, for every terminal the
            // subscription covers rather than only the kernel's.
            KernelStatus::Cwd { .. }
            | KernelStatus::CommandStarted { .. }
            | KernelStatus::CommandFinished { .. } => {}
        }
    }
}

fn input_frame(terminal_id: ResourceId, event: InputEvent) -> Option<FrameKind> {
    Some(match event {
        InputEvent::Key(event) => FrameKind::InputKey { terminal_id, event },
        InputEvent::Mouse(event) => FrameKind::InputMouse { terminal_id, event },
        InputEvent::Focus(event) => FrameKind::InputFocus { terminal_id, event },
        InputEvent::Paste(event) => FrameKind::InputPaste { terminal_id, event },
        _ => return None,
    })
}
