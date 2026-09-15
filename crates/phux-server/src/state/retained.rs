//! Retain on exit (ADR-0124): exit is a facet, close is a purge.
//!
//! A Terminal spawned with `SPAWN_RESOURCE.retain_secs` (or under the
//! operator's `defaults.retain-on-exit`) is not reaped when its process
//! exits. Its exit watcher reaps the child, records an [`ExitFacet`] here,
//! and leaves the resource in the registry as `Exited`: the wire id stays
//! valid, the engine keeps its grid and history, and `GET_STATE` reports the
//! facet. The resource leaves through the ordinary close path, with the
//! ordinary `RESOURCE_CLOSED`, when it is purged.
//!
//! Every purge is a cancellation of the resource's engine token, which the
//! retained pane's exit watcher is waiting on:
//!
//! - **Expiry.** The watcher's own deadline, `retain_secs` after the exit.
//! - **Count bound.** Retaining one more pane than
//!   `defaults.retain-on-exit-max` evicts the oldest, here, in the lock that
//!   retains the new one.
//! - **Kill.** `KILL_RESOURCE(S)` records `Killed` and cancels the token,
//!   exactly as it does for a live pane.
//! - **Shutdown.** The root token cancels every engine token.
//!
//! There is no second table of exited resources: the facet lives beside the
//! live descriptor, keyed by the same id, and is forgotten by the reap that
//! retires that id. One id never has two lifecycles.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use phux_core::ids::ResourceId;
use phux_core::process::{ExitOutcome, ProcessExit};
use phux_protocol::wire::frame::CloseReason;
use phux_protocol::wire::info::ExitFacet;
use tokio_util::sync::CancellationToken;

use super::ServerState;

/// The operator's retention settings (`defaults.retain-on-exit*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainPolicy {
    /// Retain spawns that omit `retain_secs` (`defaults.retain-on-exit`).
    pub by_default: bool,
    /// What `retain_secs = 0` and a defaulted spawn get
    /// (`defaults.retain-on-exit-secs`).
    pub default_secs: u32,
    /// The cap on any requested retention (`defaults.retain-on-exit-max-secs`).
    pub max_secs: u32,
    /// How many exited resources may be retained at once; the oldest is
    /// purged first (`defaults.retain-on-exit-max`).
    pub max_count: u32,
}

impl Default for RetainPolicy {
    fn default() -> Self {
        Self {
            by_default: false,
            default_secs: phux_config::DEFAULT_RETAIN_ON_EXIT_SECS,
            max_secs: phux_config::DEFAULT_RETAIN_ON_EXIT_MAX_SECS,
            max_count: phux_config::DEFAULT_RETAIN_ON_EXIT_MAX,
        }
    }
}

impl RetainPolicy {
    /// How long a Terminal spawned with `requested` (`SPAWN_RESOURCE` field
    /// 16) is retained after its process exits, or `None` when it is not.
    ///
    /// An absent field is retained only when the operator made retention
    /// the default; `0` asks for the server default; every value is capped.
    #[must_use]
    pub fn resolve(self, requested: Option<u32>) -> Option<u32> {
        let requested = requested.or_else(|| self.by_default.then_some(0))?;
        let secs = if requested == 0 {
            self.default_secs
        } else {
            requested
        };
        Some(secs.min(self.max_secs))
    }
}

/// What a retained pane's exit watcher holds until the purge.
#[derive(Debug)]
pub(crate) struct Retention {
    /// The pane's engine token. A kill, an eviction, and a shutdown all
    /// purge by cancelling it.
    pub(crate) token: CancellationToken,
    /// How long after the exit the pane expires.
    pub(crate) hold: Duration,
}

/// Per-pane retention bookkeeping. Every entry is keyed by a registry id and
/// forgotten by the reap that retires it.
#[derive(Debug, Default)]
pub(super) struct RetainedTable {
    /// Seconds each pane asked to be retained, resolved at spawn.
    requested: HashMap<ResourceId, u32>,
    /// The exit facet of every pane retained after its process exited.
    exited: HashMap<ResourceId, ExitFacet>,
    /// Retained panes not yet asked to leave, oldest first: the count
    /// bound's eviction order.
    order: VecDeque<ResourceId>,
}

impl RetainedTable {
    /// Drop every entry for a reaped pane.
    pub(super) fn forget(&mut self, pane: ResourceId) {
        self.requested.remove(&pane);
        self.exited.remove(&pane);
        self.order.retain(|retained| *retained != pane);
    }
}

impl ServerState {
    /// Mirror `defaults.retain-on-exit*`. Called once at startup.
    pub const fn set_retain_policy(&mut self, policy: RetainPolicy) {
        self.config.retain = policy;
    }

    /// The retention settings set by [`Self::set_retain_policy`].
    #[must_use]
    pub const fn retain_policy(&self) -> RetainPolicy {
        self.config.retain
    }

    /// Record that `pane` is retained for `secs` after its process exits.
    /// Called in the lock that registers the pane.
    pub(crate) fn note_retain_request(&mut self, pane: ResourceId, secs: u32) {
        self.retained.requested.insert(pane, secs);
    }

    /// Seconds `pane` asked to be retained after its process exits, while
    /// it has not exited yet.
    #[must_use]
    pub fn retain_request(&self, pane: ResourceId) -> Option<u32> {
        if self.retained.exited.contains_key(&pane) {
            return None;
        }
        self.retained.requested.get(&pane).copied()
    }

    /// Restore a retained pane's exit record in a resumed image (ADR-0124
    /// §6), so the close that reports it carries how its process ended.
    pub(crate) fn restore_retained_exit(&mut self, pane: ResourceId, facet: ExitFacet) {
        self.retained.exited.insert(pane, facet);
    }

    /// How a retained pane's process ended, as an exit outcome for the
    /// `RESOURCE_CLOSED` and `pane_closed` that purge it.
    #[must_use]
    pub fn retained_outcome(&self, pane: ResourceId) -> Option<ExitOutcome> {
        self.retained_exit(pane).map(|facet| ExitOutcome {
            status: facet.exit_status,
            signal: facet.signal,
        })
    }

    /// The exit facet of a retained pane whose process has exited; `None`
    /// for a live pane and for one that is not retained.
    #[must_use]
    pub fn retained_exit(&self, pane: ResourceId) -> Option<ExitFacet> {
        self.retained.exited.get(&pane).copied()
    }

    /// Keep `pane` as `Exited` instead of closing it, when it asked to be
    /// retained and nothing is already closing it.
    ///
    /// A pane with a recorded close reason (a kill or shutdown that raced
    /// its exit) is not retained: that closer asked for it to go, and the
    /// watcher closes it at once, so the race yields exactly one
    /// `RESOURCE_CLOSED`. Retaining one pane past the count bound evicts the
    /// oldest in this same lock.
    pub(crate) fn retain_exited(
        &mut self,
        pane: ResourceId,
        outcome: ExitOutcome,
        now_ms: u64,
    ) -> Option<Retention> {
        let secs = *self.retained.requested.get(&pane)?;
        let closing = self.close_reasons.contains_key(&pane);
        if closing || self.config.retain.max_count == 0 {
            return None;
        }
        self.sessions.registry.resource(pane)?;
        let token = self.resources.token(pane)?;
        let facet = ExitFacet::new(now_ms, now_ms.saturating_add(u64::from(secs) * 1000))
            .with_exit_status(outcome.status)
            .with_signal(outcome.signal);
        self.retained.exited.insert(pane, facet);
        self.retained.order.push_back(pane);
        self.evict_retained_over_bound();
        Some(Retention {
            token,
            hold: Duration::from_secs(u64::from(secs)),
        })
    }

    /// Purge the oldest retained panes until the count bound holds. Each
    /// closes through its own watcher, as `EXITED`.
    fn evict_retained_over_bound(&mut self) {
        let bound = usize::try_from(self.config.retain.max_count).unwrap_or(usize::MAX);
        while self.retained.order.len() > bound {
            let Some(oldest) = self.retained.order.pop_front() else {
                return;
            };
            self.mark_resource_closing(oldest, CloseReason::Exited);
            self.detach_resource_actor(oldest);
        }
    }

    /// The `process.exit` record `GET_TERMINAL_STATE` reports for `pane`,
    /// when the state knows better than the engine: a retained pane's exit
    /// facet, so both inspection surfaces report one record.
    #[must_use]
    pub fn retained_process_exit(&self, pane: ResourceId) -> Option<ProcessExit> {
        self.retained_exit(pane).map(|facet| process_exit(&facet))
    }

    /// Why `pane` is leaving, when a closer recorded it and the close has not
    /// been emitted yet.
    #[must_use]
    pub fn pending_close_reason(&self, pane: ResourceId) -> Option<CloseReason> {
        self.close_reasons.get(&pane).copied()
    }
}

/// The `GET_TERMINAL_STATE` `process.exit` record for an exit facet: the
/// same status, signal, reason, and time the snapshot reports.
#[must_use]
pub fn process_exit(facet: &ExitFacet) -> ProcessExit {
    ProcessExit {
        status: facet.exit_status,
        signal: facet.signal,
        reason: close_reason_name(facet.reason).to_owned(),
        exited_at_ms: Some(facet.exited_at_ms),
    }
}

/// A close reason in the snake-case vocabulary `process.exit.reason` uses
/// (L1 §6.3).
#[must_use]
pub const fn close_reason_name(reason: CloseReason) -> &'static str {
    match reason {
        CloseReason::Exited => "exited",
        CloseReason::Killed => "killed",
        CloseReason::ParentClosed => "parent_closed",
        CloseReason::ServerShutdown => "server_shutdown",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RetainPolicy {
        RetainPolicy {
            by_default: false,
            default_secs: 600,
            max_secs: 900,
            max_count: 2,
        }
    }

    #[test]
    fn an_absent_field_is_not_retained_unless_the_operator_says_so() {
        assert_eq!(policy().resolve(None), None);
        let by_default = RetainPolicy {
            by_default: true,
            ..policy()
        };
        assert_eq!(by_default.resolve(None), Some(600));
    }

    #[test]
    fn zero_is_the_default_and_every_value_is_capped() {
        assert_eq!(policy().resolve(Some(0)), Some(600));
        assert_eq!(policy().resolve(Some(30)), Some(30));
        assert_eq!(policy().resolve(Some(86_400)), Some(900));
    }

    #[test]
    fn the_process_exit_record_is_the_facet() {
        let facet = ExitFacet::new(7, 9)
            .with_signal(Some(15))
            .with_reason(CloseReason::Killed);
        let exit = process_exit(&facet);
        assert_eq!(exit.status, None);
        assert_eq!(exit.signal, Some(15));
        assert_eq!(exit.reason, "killed");
        assert_eq!(exit.exited_at_ms, Some(7));
    }

    #[test]
    fn retention_is_refused_while_a_close_is_pending_and_bounded_by_count() {
        let mut s = ServerState::new();
        s.set_retain_policy(policy());
        let mut panes = Vec::new();
        for name in ["a", "b", "c"] {
            let (_, _, pane) = s.seed_session(name);
            let token = CancellationToken::new();
            s.resources.register_token_for_test(pane, token);
            s.note_retain_request(pane, 30);
            panes.push(pane);
        }
        s.mark_resource_closing(panes[0], CloseReason::Killed);
        assert!(
            s.retain_exited(panes[0], ExitOutcome::exited(0), 1)
                .is_none(),
            "a kill that raced the exit wins"
        );
        let first = s
            .retain_exited(panes[1], ExitOutcome::exited(3), 1)
            .expect("retained");
        let facet = s.retained_exit(panes[1]).expect("facet");
        assert_eq!(facet.exit_status, Some(3));
        assert_eq!(facet.retained_until_ms, 30_001);
        assert_eq!(first.hold, Duration::from_secs(30));
        assert!(!first.token.is_cancelled());
        let _ = s
            .retain_exited(panes[2], ExitOutcome::signaled(9), 2)
            .expect("retained");
        assert!(!first.token.is_cancelled(), "two fit under a bound of two");
        let (_, _, fourth) = s.seed_session("d");
        s.resources
            .register_token_for_test(fourth, CancellationToken::new());
        s.note_retain_request(fourth, 30);
        let _ = s.retain_exited(fourth, ExitOutcome::exited(0), 3);
        assert!(first.token.is_cancelled(), "the oldest is evicted");
        assert_eq!(s.pending_close_reason(panes[1]), Some(CloseReason::Exited));
    }
}
