//! What the server knows about the *agents* running inside its panes: the
//! pending-question detector (ADR-0046 §D) and the `phux.agent/v1` record
//! arbiter (ADR-0046 §E).
//!
//! Two fields that were flat on [`super::ServerState`] live here because
//! they share one lifetime — a *pane's* existence. Both are populated only
//! while an agent is live in a pane and both must be dropped in
//! `ServerState::reap_terminal`'s cascade, or a recycled id inherits a
//! stale question or a stale ownership claim. Before this grouping the
//! asked-detector was the one field in `state` that `state::reap` reached
//! into directly, with no wrapping method; both now clear through this
//! type.
//!
//! # Ownership boundary
//!
//! This type owns the two ledgers, not the agent protocol around them. The
//! detect-and-publish path (`SET_METADATA` arbitration, the
//! `phux.agent.asked/v1` broadcast, the hook dispatch) lives in
//! `runtime::client` / `runtime::commands` and reaches in through the
//! delegating accessors on `ServerState` (see `state::agent`). The two
//! ledgers are keyed differently on purpose and that is not incidental:
//! the detector is keyed by core [`TerminalId`] because it is fed by pane
//! output, while the arbiter is keyed by *wire* terminal id because it
//! mirrors the per-Terminal L3 metadata scope. `state::reap` therefore
//! clears the detector before retiring the wire id and the arbiter after,
//! inside the `retire_terminal` binding — the ordering the comment there
//! explains.
//!
//! Both fields are private: like `state::lease_table`, nothing on
//! `ServerState` needs to borrow-split one of these against another field,
//! so every read and write goes through a method here.
//!
//! Nothing here is `async` and nothing awaits, so the state lock can never
//! be held across a suspension point through this type.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState`, so the crate's public surface is unchanged
//! and both ledgers stay exactly as unreachable from outside `state` as
//! they were as private fields.
//!
//! # Why this file is not `state/agent_state.rs`
//!
//! [`crate::agent_state`] already exists as a crate-level module — it is
//! where [`AgentRecordArbiter`] itself lives. A `state::agent_state`
//! sibling would resolve fine (both paths are absolute) but would read as
//! the same module at every use site, so the file is named for the concern
//! instead of for the struct.

use phux_core::ids::TerminalId;
use phux_protocol::ids::TerminalId as WireTerminalId;

use crate::agent_asked::{AskedDetector, AskedPayload, AskedSource, AskedTransition};
use crate::agent_state::AgentRecordArbiter;

/// Both per-pane agent ledgers the server owns.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct AgentState {
    /// Which panes currently have an agent waiting on a human
    /// (`phux.agent.asked/v1`, ADR-0046 §D). Fed by the hook bridge and by
    /// the server-side output detector; cleared when the question is
    /// answered or the pane is reaped.
    asked: AskedDetector,
    /// Who owns each Terminal's `phux.agent/v1` record: a human's explicit
    /// `SET_METADATA`, or the server-side detector (ADR-0046 §E). An
    /// explicit declaration of `state` outranks the detector, which stands
    /// down until the record is deleted.
    records: AgentRecordArbiter,
}

impl Default for AgentState {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentState {
    /// Build empty ledgers — no pane has a pending question and no record
    /// has an owner.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            asked: AskedDetector::default(),
            records: AgentRecordArbiter::default(),
        }
    }

    /// Record that `terminal`'s agent is asking, returning how that moved
    /// the pane's pending-question state.
    pub(super) fn report_asked(
        &mut self,
        terminal: TerminalId,
        source: AskedSource,
        payload: AskedPayload,
    ) -> AskedTransition {
        self.asked.report(terminal, source, payload)
    }

    /// The question `terminal`'s agent is currently waiting on, if any.
    #[cfg(test)]
    pub(super) fn current_asked(&self, terminal: TerminalId) -> Option<&AskedPayload> {
        self.asked.current(terminal)
    }

    /// Drop any pending question for a pane that is going away.
    ///
    /// Keyed by core [`TerminalId`], so `state::reap` calls this *before*
    /// the wire id is retired; the arbiter half ([`Self::forget_record`])
    /// is keyed by wire id and is cleared after.
    pub(super) fn clear_asked(&mut self, terminal: TerminalId) {
        self.asked.clear_terminal(terminal);
    }

    /// Read the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(super) const fn records(&self) -> &AgentRecordArbiter {
        &self.records
    }

    /// Mutate the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(super) const fn records_mut(&mut self) -> &mut AgentRecordArbiter {
        &mut self.records
    }

    /// Drop every arbiter trace of a retired wire terminal id.
    ///
    /// The record died with the per-Terminal metadata scope; the arbiter's
    /// bookkeeping about who owned it must not outlive it, or a recycled
    /// wire id would inherit a stale declaration.
    pub(super) fn forget_record(&mut self, terminal: &WireTerminalId) {
        self.records.forget(terminal);
    }
}
