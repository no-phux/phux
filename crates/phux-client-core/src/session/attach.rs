//! ATTACH inventory: membership, release, and readiness tracking.

use super::{
    AttachParticipant, AttachState, EffectBuffer, EngineAdapter, KernelDamage, KernelDamageKind,
    KernelEffect, KernelError, KernelSend, ResourceId, ResourceKind, SessionKernel,
};

impl<E: EngineAdapter> SessionKernel<E> {
    /// Whether the active ATTACH inventory authorizes one open terminal.
    #[must_use]
    pub fn active_attach_contains(&self, terminal_id: &ResourceId) -> bool {
        !self.closed.contains(terminal_id)
            && self.attach.as_ref().is_some_and(|attach| {
                attach
                    .terminals
                    .iter()
                    .any(|participant| &participant.terminal_id == terminal_id)
            })
    }

    /// Number of attach participants that have not reached a per-terminal
    /// READY or CLOSED boundary. Multi-stream transports can deliver the
    /// aggregate `ATTACH_READY` on control before the final Terminal stream
    /// READY, so the TUI uses this query to preserve the kernel's barrier.
    #[must_use]
    pub fn attach_ready_pending(&self) -> Option<usize> {
        self.attach
            .as_ref()
            .filter(|attach| !attach.released)
            .map(|attach| {
                attach
                    .terminals
                    .iter()
                    .filter(|participant| !participant.resolved)
                    .count()
            })
    }

    /// Release the active ATTACH inventory after the connection is detached.
    pub fn release_active_attach(&mut self) {
        self.attach = None;
    }

    /// Complete an acknowledged per-terminal detach after the session ATTACH
    /// barrier. The caller must revoke dynamic admission and reject subsequent
    /// unsolicited frames. This transition withdraws initial participation; it
    /// never marks durable work closed and permits a later explicit reattach.
    /// Returns false without mutation while the initial barrier is outstanding.
    #[must_use]
    pub fn detach_terminal(&mut self, terminal_id: &ResourceId) -> bool {
        if let Some(attach) = self.attach.as_mut() {
            if !attach.released {
                return false;
            }
            attach
                .terminals
                .retain(|entry| &entry.terminal_id != terminal_id);
        }
        self.release_terminal(terminal_id)
    }

    /// Release all replica, retirement, and echo-probe state for a subscription.
    ///
    /// The caller MUST revoke its terminal admission before calling this method
    /// and reject subsequent unsolicited frames for the released terminal. Unlike
    /// terminal closure, release permits a later explicitly requested subscription
    /// to the same ID. The caller owns removal of its presentation and queued
    /// effects; this method emits no effects.
    ///
    /// Returns `false` without mutation for any terminal in the active ATTACH
    /// inventory, including closed participants, preserving its aggregate barrier.
    /// Returns `true` otherwise, including for an already released terminal.
    #[must_use]
    pub fn release_terminal(&mut self, terminal_id: &ResourceId) -> bool {
        if self.attach.as_ref().is_some_and(|attach| {
            attach
                .terminals
                .iter()
                .any(|participant| &participant.terminal_id == terminal_id)
        }) {
            return false;
        }
        self.terminals.remove(terminal_id);
        self.closed_replicas.remove(terminal_id);
        self.retain_replica_on_close.remove(terminal_id);
        self.agents.remove(terminal_id);
        self.closed_native_views.remove(terminal_id);
        self.kinds.remove(terminal_id);
        self.closed.remove(terminal_id);
        self.exit_reported.remove(terminal_id);
        self.perf_echo.forget(terminal_id);
        true
    }

    /// Forget what only the previous server incarnation could mean: every
    /// `AgentSession` resource, open or closed, and every closed resource's
    /// tombstone, as [`Self::release_terminal`] forgets one. A new
    /// incarnation may reuse those ids for a resource of any kind.
    pub fn release_incarnation(&mut self) {
        let stale: Vec<ResourceId> = self
            .kinds
            .iter()
            .filter(|(_, kind)| **kind == ResourceKind::AgentSession)
            .map(|(terminal_id, _)| terminal_id.clone())
            .chain(self.closed.iter().cloned())
            .collect();
        for terminal_id in stale {
            let _ = self.release_terminal(&terminal_id);
        }
    }

    pub(super) fn attach_started(
        &mut self,
        attach_id: u32,
        terminal_ids: &[ResourceId],
    ) -> Result<(), KernelError<E::Error>> {
        if let Some(attach) = self.attach.as_ref()
            && !attach.released
        {
            return Err(KernelError::AttachInProgress {
                active_attach_id: attach.attach_id,
            });
        }
        for (index, terminal_id) in terminal_ids.iter().enumerate() {
            if self.closed.contains(terminal_id) {
                return Err(KernelError::ClosedTerminal(terminal_id.clone()));
            }
            if terminal_ids[..index].contains(terminal_id) {
                return Err(KernelError::DuplicateAttachResource(terminal_id.clone()));
            }
        }

        for terminal_id in terminal_ids {
            self.expect_kind(terminal_id, ResourceKind::Terminal)?;
        }
        self.terminals.reserve(terminal_ids.len());
        for terminal_id in terminal_ids {
            self.terminals.entry(terminal_id.clone()).or_default();
        }

        let attach = self.attach.get_or_insert_with(|| AttachState {
            attach_id,
            released: false,
            terminals: Vec::with_capacity(terminal_ids.len()),
        });
        attach.attach_id = attach_id;
        attach.released = false;
        attach.terminals.clear();
        attach.terminals.reserve(terminal_ids.len());
        attach
            .terminals
            .extend(
                terminal_ids
                    .iter()
                    .cloned()
                    .map(|terminal_id| AttachParticipant {
                        terminal_id,
                        resolved: false,
                        pending_removal: false,
                    }),
            );
        Ok(())
    }

    pub(super) fn attach_ready(
        &mut self,
        attach_id: u32,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let Some(attach) = self.attach.as_mut() else {
            return Err(KernelError::AttachIdMismatch {
                expected: None,
                actual: attach_id,
            });
        };
        if attach.attach_id != attach_id {
            return Err(KernelError::AttachIdMismatch {
                expected: Some(attach.attach_id),
                actual: attach_id,
            });
        }
        let remaining = attach
            .terminals
            .iter()
            .filter(|participant| !participant.resolved)
            .count();
        if remaining != 0 {
            return Err(KernelError::AttachNotReady { remaining });
        }
        if attach.released {
            return Err(KernelError::AttachIdMismatch {
                expected: None,
                actual: attach_id,
            });
        }

        attach.released = true;
        effects.push(KernelEffect::Send(KernelSend::SubscribeEvents {
            terminal: None,
            after_seq: None,
        }));
        for participant in &attach.terminals {
            if participant.pending_removal {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: participant.terminal_id.clone(),
                    kind: KernelDamageKind::Removed,
                }));
                continue;
            }
            if self
                .terminals
                .get(&participant.terminal_id)
                .and_then(|state| state.published.as_ref())
                .is_some()
            {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: participant.terminal_id.clone(),
                    kind: KernelDamageKind::Full,
                }));
            }
        }
        Ok(())
    }

    pub(super) fn ensure_open(
        &self,
        terminal_id: &ResourceId,
    ) -> Result<(), KernelError<E::Error>> {
        if self.closed.contains(terminal_id) {
            Err(KernelError::ClosedTerminal(terminal_id.clone()))
        } else {
            Ok(())
        }
    }

    pub(super) fn attach_blocks(&self, terminal_id: &ResourceId) -> bool {
        self.attach.as_ref().is_some_and(|attach| {
            !attach.released
                && attach
                    .terminals
                    .iter()
                    .any(|participant| &participant.terminal_id == terminal_id)
        })
    }

    pub(super) fn mark_attach_resolved(&mut self, terminal_id: &ResourceId) {
        if let Some(participant) = self.attach.as_mut().and_then(|attach| {
            attach
                .terminals
                .iter_mut()
                .find(|participant| &participant.terminal_id == terminal_id)
        }) {
            participant.resolved = true;
            participant.pending_removal = false;
        }
    }

    pub(super) fn mark_attach_unresolved(
        &mut self,
        terminal_id: &ResourceId,
        pending_removal: bool,
    ) {
        if let Some(participant) = self.attach.as_mut().and_then(|attach| {
            attach
                .terminals
                .iter_mut()
                .find(|participant| &participant.terminal_id == terminal_id)
        }) {
            participant.resolved = false;
            participant.pending_removal |= pending_removal;
        }
    }

    pub(super) fn mark_attach_closed(&mut self, terminal_id: &ResourceId, pending_removal: bool) {
        if let Some(participant) = self.attach.as_mut().and_then(|attach| {
            attach
                .terminals
                .iter_mut()
                .find(|participant| &participant.terminal_id == terminal_id)
        }) {
            participant.resolved = true;
            participant.pending_removal |= pending_removal;
        }
    }
}
