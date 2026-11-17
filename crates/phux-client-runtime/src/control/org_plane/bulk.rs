//! Bulk moves: a `move_resources` run split into journaled chunks, each
//! submitted only after the previous chunk's receipt and a fresh inventory.

use std::collections::VecDeque;
use std::time::Instant;

use phux_client_core::org_ops::{OrgOutcome, OrgReport, SubmitError};
use phux_protocol::ids::{AuthorityId, OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Command, MoveItem, OrgFilter};

use super::super::{ControlPlane, Event};

/// Only the current chunk enters the retry journal. Later payloads are built
/// after a post-receipt inventory read, never by changing a retry's payload.
#[derive(Debug)]
pub(super) struct BulkMove {
    pub(super) authority: AuthorityId,
    pub(super) to: SessionKey,
    pub(super) remaining: VecDeque<(OperationId, Vec<ResourceKey>)>,
    pub(super) phase: BulkPhase,
    /// Bounds inventory waits; an attempted command keeps the journal's
    /// independent receipt/retry semantics rather than being called not sent.
    pub(super) deadline: Instant,
}

#[derive(Debug)]
pub(super) enum BulkPhase {
    Submitted(OperationId),
    NeedsSeed,
    Seeding,
    Ready,
}

impl ControlPlane {
    pub(super) fn resolve_bulk_move(&mut self, report: &OrgReport) -> Option<BulkMove> {
        let index = self.org.moves.iter().position(
            |run| matches!(run.phase, BulkPhase::Submitted(id) if id == report.operation),
        )?;
        if matches!(&report.outcome, OrgOutcome::Committed(receipt) if receipt.operation == report.operation)
            && !self.org.moves[index].remaining.is_empty()
        {
            self.org.moves[index].phase = BulkPhase::NeedsSeed;
            None
        } else {
            Some(self.org.moves.remove(index))
        }
    }

    pub(super) fn bulk_moves_seeded(&mut self) {
        for run in &mut self.org.moves {
            if matches!(run.phase, BulkPhase::Seeding) {
                run.phase = BulkPhase::Ready;
            }
        }
    }

    pub(super) fn cancel_bulk_tail(&mut self, run: BulkMove, why: &str) {
        self.org.deferred -= run.remaining.len();
        for (operation, _) in run.remaining {
            self.push_event(Event::OrganizationOperation {
                operation,
                outcome: OrgOutcome::NotSent {
                    reason: why.to_owned(),
                },
            });
        }
    }

    pub(super) fn cancel_bulk_moves(&mut self, why: &str) {
        for run in std::mem::take(&mut self.org.moves) {
            self.cancel_bulk_tail(run, why);
        }
    }

    pub(super) fn expire_bulk_reads(&mut self, now: Instant) {
        let (running, expired): (Vec<_>, Vec<_>) = std::mem::take(&mut self.org.moves)
            .into_iter()
            .partition(|run| matches!(run.phase, BulkPhase::Submitted(_)) || now < run.deadline);
        self.org.moves = running;
        for run in expired {
            self.cancel_bulk_tail(run, "the bulk move inventory deadline passed");
        }
    }

    pub(super) fn advance_bulk_moves(&mut self) {
        self.expire_bulk_reads(Instant::now());
        if !self.handshake_ready || !self.org.ready || !self.organization.is_live() {
            return;
        }
        let mut running = Vec::new();
        for mut run in std::mem::take(&mut self.org.moves) {
            if self.organization.authority() != Some(run.authority) {
                self.cancel_bulk_tail(run, "the server now serves a different authority");
                continue;
            }
            match run.phase {
                BulkPhase::NeedsSeed => {
                    run.phase = BulkPhase::Seeding;
                    self.queue_org_seed(OrgFilter::All);
                }
                BulkPhase::Ready => {
                    if !self.organization.is_live() {
                        running.push(run);
                        continue;
                    }
                    if !self.submit_bulk_chunk(&mut run) {
                        continue;
                    }
                }
                BulkPhase::Submitted(_) | BulkPhase::Seeding => {}
            }
            running.push(run);
        }
        self.org.moves = running;
        self.flush_org_journal();
    }

    pub(super) fn submit_bulk_chunk(&mut self, run: &mut BulkMove) -> bool {
        let Some((operation, resources)) = run.remaining.front() else {
            return false;
        };
        let operation = *operation;
        // Transfer this reserved slot to the journal even while the run is
        // temporarily removed from the lane during advancement.
        self.org.deferred -= 1;
        let result = self
            .bulk_move_command(operation, resources, run.to)
            .and_then(|command| self.journal_org_operation(command));
        if let Err(error) = result {
            self.org.deferred -= run.remaining.len() - 1;
            for (operation, _) in run.remaining.drain(..) {
                self.push_event(Event::OrganizationOperation {
                    operation,
                    outcome: OrgOutcome::NotSent {
                        reason: error.to_string(),
                    },
                });
            }
            return false;
        }
        run.remaining.pop_front();
        run.phase = BulkPhase::Submitted(operation);
        true
    }

    pub(super) fn bulk_move_command(
        &self,
        operation: OperationId,
        resources: &[ResourceKey],
        to: SessionKey,
    ) -> Result<Command, SubmitError> {
        let to_expected = self.session_record(&to)?.revision;
        let items = resources
            .iter()
            .map(|resource| {
                let from = self
                    .organization
                    .resource(resource)
                    .ok_or(SubmitError::NotAnOperation)?
                    .session;
                Ok(MoveItem {
                    resource: *resource,
                    from,
                    from_expected: self.session_record(&from)?.revision,
                })
            })
            .collect::<Result<Vec<_>, SubmitError>>()?;
        Ok(Command::ResourceMoveMany {
            operation,
            items,
            to,
            to_expected,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]

    use std::time::Duration;

    use super::super::mint_operation;
    use super::*;
    use crate::control::ControlOptions;

    #[test]
    fn inventory_deadline_is_scheduled_and_cancels_only_the_never_sent_tail() {
        let mut plane = ControlPlane::new(ControlOptions::default());
        let first = mint_operation();
        let tail = mint_operation();
        let deadline = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        plane.org.moves.push(BulkMove {
            authority: AuthorityId::new([1; 16]).unwrap(),
            to: SessionKey::new([2; 16]).unwrap(),
            remaining: VecDeque::from([(tail, vec![ResourceKey::new([3; 16]).unwrap()])]),
            phase: BulkPhase::Submitted(first),
            deadline,
        });
        plane.org.deferred = 1;
        // An attempted command retains independent journal semantics. The
        // inventory timer is armed only once its receipt has committed.
        assert!(plane.next_operation_deadline().is_none());
        plane.org.moves[0].phase = BulkPhase::Seeding;
        assert_eq!(plane.next_operation_deadline(), Some(deadline));
        assert!(plane.expire_operations());
        assert!(matches!(plane.take_events().as_slice(),
            [Event::OrganizationOperation { operation, outcome: OrgOutcome::NotSent { .. } }] if *operation == tail));
        assert_eq!(plane.org.deferred, 0);
        assert!(plane.next_operation_deadline().is_none());
        assert!(!plane.expire_operations());
        plane.bulk_moves_seeded(); // A late page cannot resurrect timed-out work.
        assert!(plane.org.moves.is_empty());
    }
}
