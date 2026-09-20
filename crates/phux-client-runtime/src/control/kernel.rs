//! Engine outcomes and kernel effect processing.

use super::{
    ControlError, ControlPlane, EngineEvent, EngineOutcome, Event, FrameKind, InputEvent,
    KernelEffect, KernelSend, KernelStatus, MAX_INPUT_TERMINAL_REPLY_BYTES, ServerFeature,
};

impl ControlPlane {
    // ----- the kernel --------------------------------------------------

    pub(super) fn apply_engine(&mut self, event: EngineEvent) -> Result<(), ControlError> {
        let Some(engine) = &self.engine else {
            return Err(ControlError::Protocol(
                "stateful frame arrived before the session kernel was initialized".to_owned(),
            ));
        };
        // A frame for a terminal the kernel already closed is stale
        // evidence, not an error; only a close itself is idempotent there.
        if !matches!(event, EngineEvent::Closed { .. })
            && event
                .terminal_id()
                .is_some_and(|terminal_id| engine.is_closed(terminal_id))
        {
            return Ok(());
        }
        let outcome = engine
            .apply(event)
            .map_err(|error| ControlError::Protocol(error.to_string()))?;
        self.process_outcome(outcome, true)
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
                return Err(ControlError::Protocol(format!("session kernel: {error}")));
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
            KernelEffect::AgentRecords {
                terminal_id,
                records,
            } => self.push_event(Event::AgentRecords {
                terminal_id,
                records,
            }),
        }
    }

    pub(super) fn process_send(&mut self, send: KernelSend) {
        let frame = match send {
            KernelSend::Input { terminal_id, event } => match event {
                InputEvent::Key(event) => FrameKind::InputKey { terminal_id, event },
                InputEvent::Mouse(event) => FrameKind::InputMouse { terminal_id, event },
                InputEvent::Focus(event) => FrameKind::InputFocus { terminal_id, event },
                InputEvent::Paste(event) => FrameKind::InputPaste { terminal_id, event },
                _ => return,
            },
            KernelSend::PtyWrite { terminal_id, bytes } => {
                if !self.server_has(ServerFeature::TerminalReply) {
                    tracing::warn!(
                        "terminal query reply not sent: server lacks terminal-reply support"
                    );
                    return;
                }
                if bytes.is_empty() || bytes.len() > MAX_INPUT_TERMINAL_REPLY_BYTES {
                    tracing::warn!("terminal reply is empty or exceeds the protocol byte limit");
                    return;
                }
                FrameKind::InputTerminalReply {
                    terminal_id,
                    bytes: bytes.into(),
                }
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
                phux_client_core::engine::EngineStatus::Title(title) => {
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
            } => self.push_event(Event::Exited {
                terminal_id,
                exit_status,
                signal,
                reason,
            }),
            // Folded from the agent event itself, for every terminal the
            // subscription covers rather than only the kernel's.
            KernelStatus::Cwd { .. }
            | KernelStatus::CommandStarted { .. }
            | KernelStatus::CommandFinished { .. } => {}
        }
    }
}
