//! Engine-effect translation, closure, and frontend actions.

use super::lane::ResourceLane;
use super::{
    AgentEvent, CloseReason, ClosedReplica, ControlAction, EffectBuffer, EngineAdapter,
    EngineDamage, EngineEffect, EngineEffectBuffer, InputEligibility, InputEvent, KernelAction,
    KernelDamage, KernelDamageKind, KernelEffect, KernelError, KernelJob, KernelSend, KernelStatus,
    ReplicaKey, ResourceId, SessionKernel,
};

/// Translate everything the engine just produced into kernel effects,
/// keeping the buffer allocation for the next apply.
pub(super) fn drain_engine_effects<E: EngineAdapter>(
    engine_effects: &mut EngineEffectBuffer,
    replica_key: &ReplicaKey,
    damage_allowed: bool,
    effects: &mut EffectBuffer,
) {
    for effect in engine_effects.drain() {
        SessionKernel::<E>::translate_engine_effect(replica_key, effect, damage_allowed, effects);
    }
}

impl<E: EngineAdapter> SessionKernel<E> {
    pub(super) fn translate_engine_effect(
        key: &ReplicaKey,
        effect: EngineEffect,
        damage_allowed: bool,
        effects: &mut EffectBuffer,
    ) {
        match effect {
            EngineEffect::Damage(damage) if damage_allowed => {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: key.terminal_id.clone(),
                    kind: match damage {
                        EngineDamage::Full => KernelDamageKind::Full,
                        EngineDamage::Rows { first, last } => {
                            KernelDamageKind::Rows { first, last }
                        }
                    },
                }));
            }
            EngineEffect::Damage(_) => {}
            EngineEffect::Status(status) => {
                effects.push(KernelEffect::Status(KernelStatus::Engine {
                    key: key.clone(),
                    status,
                }));
            }
            EngineEffect::Job(job) => {
                effects.push(KernelEffect::Job(KernelJob {
                    key: key.clone(),
                    job,
                }));
            }
        }
    }

    pub(super) fn terminal_closed(
        &mut self,
        terminal_id: &ResourceId,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: CloseReason,
        effects: &mut EffectBuffer,
    ) {
        self.perf_echo.forget(terminal_id);
        self.unsupported.remove(terminal_id);
        let lane = self.lanes.remove(terminal_id);
        // Only tracked Terminals get a process-exit status: agent sessions report
        // via `AgentSessionStatus`, and a forgotten id has nothing new to say.
        let was_tracked_terminal = matches!(lane, Some(ResourceLane::Terminal(_)));
        let damage_blocked = self.attach_blocks(terminal_id);
        let published = match lane {
            Some(ResourceLane::Terminal(state)) => state.published,
            Some(ResourceLane::Agent(stream)) => {
                if let Some(mut view) = stream.native_view().cloned() {
                    view.connected = false;
                    view.questions.clear();
                    view.turn_id = None;
                    self.closed_native_views.insert(terminal_id.clone(), view);
                }
                None
            }
            Some(ResourceLane::Document(_)) | None => None,
        };
        let had_published = published.is_some();
        if self.retain_replica_on_close.remove(terminal_id)
            && let Some(replica) = published
        {
            self.closed_replicas
                .entry(terminal_id.clone())
                .or_insert_with(|| ClosedReplica {
                    key: replica.key,
                    geometry: replica.geometry,
                    last_seq: replica.last_seq,
                    engine: replica.engine,
                    history: replica.history,
                });
        }
        self.closed.insert(terminal_id.clone());
        self.mark_attach_closed(terminal_id, had_published && damage_blocked);
        if was_tracked_terminal
            && let Some(status) = self.exited_status_once(terminal_id, exit_status, signal, reason)
        {
            effects.push(KernelEffect::Status(status));
        }
        if had_published && !damage_blocked {
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Removed,
            }));
        }
    }

    pub(super) fn action(
        &self,
        action: &KernelAction<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        match *action {
            KernelAction::Input { terminal_id, event } => {
                let eligibility = self.input_eligibility(terminal_id);
                if let InputEligibility::Ineligible(reason) = eligibility {
                    return Err(KernelError::InputIneligible {
                        terminal_id: terminal_id.clone(),
                        reason,
                    });
                }
                // A key or paste is something the program answers; mouse and
                // focus are not, and would pair with unrelated output.
                if matches!(event, InputEvent::Key(_) | InputEvent::Paste(_)) {
                    self.perf_echo.arm(terminal_id);
                }
                effects.push(KernelEffect::Send(KernelSend::Input {
                    terminal_id: terminal_id.clone(),
                    event: event.clone(),
                }));
                Ok(())
            }
        }
    }

    /// The frontend status one [`AgentEvent`] produces, or `None` for kinds
    /// surfaced elsewhere (bell, title) or not at all.
    pub(super) fn status_for_agent_event(
        &mut self,
        terminal_id: &ResourceId,
        event: &AgentEvent,
    ) -> Option<KernelStatus> {
        match event {
            AgentEvent::CwdChanged { cwd } => Some(KernelStatus::Cwd {
                terminal_id: terminal_id.clone(),
                cwd: cwd.clone(),
            }),
            AgentEvent::CommandStarted => Some(KernelStatus::CommandStarted {
                terminal_id: terminal_id.clone(),
            }),
            AgentEvent::CommandFinished { exit_code } => Some(KernelStatus::CommandFinished {
                terminal_id: terminal_id.clone(),
                exit_code: *exit_code,
            }),
            // `action` marks the transition once; `lifecycle` is restamped on
            // every later lease change (ADR-0033).
            AgentEvent::TerminalControl {
                action: ControlAction::Exited,
                exit_status,
                ..
            } => self.exited_status_once(terminal_id, *exit_status, None, CloseReason::Exited),
            _ => None,
        }
    }

    /// [`KernelStatus::Exited`] for `terminal_id` at most once; later reports
    /// (even a richer `RESOURCE_CLOSED`) are suppressed.
    fn exited_status_once(
        &mut self,
        terminal_id: &ResourceId,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: CloseReason,
    ) -> Option<KernelStatus> {
        if !self.exit_reported.insert(terminal_id.clone()) {
            return None;
        }
        Some(KernelStatus::Exited {
            terminal_id: terminal_id.clone(),
            exit_status,
            signal,
            reason,
        })
    }
}
