//! The hub's incarnation fence and actor correlation for the keyed
//! operations it forwards to one satellite (`docs/spec/L1.md` §9.1).
//!
//! A satellite owns the dedupe of a keyed operation it runs (`APPLY_INPUT`,
//! a keyed kill or signal), and that record lives in the satellite's memory:
//! it dies with the process, which is what a changed `HELLO_OK.server_id`
//! means (ADR-0053 item 5). A consumer behind a hub sees only the hub's
//! `server_id`, so it cannot tell that the satellite restarted between two
//! attempts, and a retry forwarded blindly could run twice. The hub
//! therefore records, for each operation id it forwards, the incarnation it
//! forwarded to, and answers a retry that would cross a restart with
//! `INCARNATION_CHANGED` instead of forwarding it.
//!
//! The same record names the hub consumer that sent the operation, so when
//! the hub re-stamps an event the operation caused on the satellite, the
//! event names that consumer rather than the link every consumer shares.
//!
//! Bounded like the dedupe record it fronts: the same ten-minute horizon
//! from admission and the same entry cap, and a full record refuses a new id
//! rather than evicting a live one. One record per satellite, shared across
//! the link's reconnects, since a reconnect is exactly when it matters.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::Command;

use crate::runtime::operation_dedupe::{
    DEDUPE_MAX_ENTRIES, DEDUPE_RETENTION, OperationDomain, OperationKey,
};
use crate::state::ClientId;

/// A satellite incarnation: its `HELLO_OK.server_id`, or `None` when the
/// link could not read one (which fences as a single incarnation).
pub(crate) type Incarnation = Option<[u8; 16]>;

/// What the fence says about forwarding one keyed operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FenceVerdict {
    /// Forward it: the id is new, or was forwarded to this same incarnation.
    Forward,
    /// Refuse it: the id was forwarded to an incarnation that is gone.
    IncarnationChanged,
    /// Refuse it: the record is full and the id is new.
    Full,
}

#[derive(Debug)]
struct FencedOperation {
    incarnation: Incarnation,
    actor: ClientId,
    admitted_at: Instant,
}

#[derive(Debug, Default)]
struct FenceStore {
    entries: HashMap<OperationKey, FencedOperation>,
    expiry: VecDeque<(Instant, OperationKey)>,
}

impl FenceStore {
    fn prune_expired(&mut self, now: Instant) {
        while let Some((admitted_at, key)) = self.expiry.front().copied() {
            if now.saturating_duration_since(admitted_at) < DEDUPE_RETENTION {
                break;
            }
            self.expiry.pop_front();
            if self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.admitted_at == admitted_at)
            {
                self.entries.remove(&key);
            }
        }
    }

    fn admit(
        &mut self,
        key: OperationKey,
        incarnation: Incarnation,
        actor: ClientId,
        now: Instant,
    ) -> FenceVerdict {
        self.prune_expired(now);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.incarnation != incarnation {
                return FenceVerdict::IncarnationChanged;
            }
            // A retry from a reconnected consumer arrives under a new
            // connection; the events still to come are its.
            entry.actor = actor;
            return FenceVerdict::Forward;
        }
        if self.entries.len() >= DEDUPE_MAX_ENTRIES {
            return FenceVerdict::Full;
        }
        self.entries.insert(
            key,
            FencedOperation {
                incarnation,
                actor,
                admitted_at: now,
            },
        );
        self.expiry.push_back((now, key));
        FenceVerdict::Forward
    }
}

/// One satellite's fence. Cloning shares it.
#[derive(Debug, Clone, Default)]
pub(crate) struct OperationFence(Arc<Mutex<FenceStore>>);

impl OperationFence {
    fn lock(&self) -> std::sync::MutexGuard<'_, FenceStore> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Admit forwarding the operation `key` from hub consumer `actor` to the
    /// satellite incarnation the link is connected to.
    pub(crate) fn admit(
        &self,
        key: OperationKey,
        incarnation: Incarnation,
        actor: ClientId,
        now: Instant,
    ) -> FenceVerdict {
        self.lock().admit(key, incarnation, actor, now)
    }

    /// The hub consumer whose keyed operation stamped `operation_id` on an
    /// event the satellite sent, while the record still holds it.
    pub(crate) fn actor_for_event(&self, operation_id: &IdempotencyKey) -> Option<ClientId> {
        let key = OperationKey::new(OperationDomain::Signal, *operation_id.as_bytes());
        self.lock().entries.get(&key).map(|entry| entry.actor)
    }
}

/// The key the fence records a forwarded command under: an `APPLY_INPUT`'s
/// operation id, or a keyed supervisory command's `operation_id`, each in
/// its own dedupe domain. `None` for a command that carries neither.
pub(crate) fn fenced_key(command: &Command) -> Option<OperationKey> {
    match command {
        Command::ApplyInput { operation_id, .. } => Some(OperationKey::new(
            OperationDomain::Input,
            *operation_id.as_bytes(),
        )),
        _ => command
            .idempotency_key()
            .map(|key| OperationKey::new(OperationDomain::Signal, *key.as_bytes())),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(value: u64) -> OperationKey {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        OperationKey::new(OperationDomain::Signal, bytes)
    }

    const A: Incarnation = Some([1; 16]);
    const B: Incarnation = Some([2; 16]);

    #[test]
    fn a_retry_within_one_incarnation_forwards_and_across_a_restart_does_not() {
        let fence = OperationFence::default();
        let now = Instant::now();
        assert_eq!(
            fence.admit(key(1), A, ClientId(7), now),
            FenceVerdict::Forward
        );
        assert_eq!(
            fence.admit(key(1), A, ClientId(8), now),
            FenceVerdict::Forward,
            "a retry to the same satellite process is the satellite's to dedupe"
        );
        assert_eq!(
            fence.admit(key(1), B, ClientId(8), now),
            FenceVerdict::IncarnationChanged
        );
        assert_eq!(
            fence.admit(key(2), B, ClientId(8), now),
            FenceVerdict::Forward,
            "a new id is new to the new incarnation too"
        );
    }

    #[test]
    fn the_newest_sender_owns_the_events_still_to_come() {
        let fence = OperationFence::default();
        let now = Instant::now();
        let id = IdempotencyKey::new([9; 16]).expect("non-zero");
        let key = OperationKey::new(OperationDomain::Signal, [9; 16]);
        assert_eq!(fence.actor_for_event(&id), None);
        fence.admit(key, A, ClientId(3), now);
        assert_eq!(fence.actor_for_event(&id), Some(ClientId(3)));
        fence.admit(key, A, ClientId(4), now);
        assert_eq!(fence.actor_for_event(&id), Some(ClientId(4)));
    }

    #[test]
    fn the_fence_is_bounded_and_shares_the_dedupe_horizon() {
        let mut store = FenceStore::default();
        let start = Instant::now();
        for value in 0..u64::try_from(DEDUPE_MAX_ENTRIES).expect("fits") {
            assert_eq!(
                store.admit(key(value), A, ClientId(1), start),
                FenceVerdict::Forward
            );
        }
        let extra = u64::try_from(DEDUPE_MAX_ENTRIES).expect("fits");
        assert_eq!(
            store.admit(key(extra), A, ClientId(1), start),
            FenceVerdict::Full,
            "a full fence refuses a new id instead of evicting a live one"
        );
        let inside = start
            + DEDUPE_RETENTION
                .checked_sub(Duration::from_secs(1))
                .expect("retention exceeds one second");
        assert_eq!(
            store.admit(key(0), B, ClientId(1), inside),
            FenceVerdict::IncarnationChanged,
            "inside the horizon the old incarnation still fences"
        );
        let past = start + DEDUPE_RETENTION;
        assert_eq!(
            store.admit(key(0), B, ClientId(1), past),
            FenceVerdict::Forward,
            "past the horizon the id is unknown, as it is to the dedupe record"
        );
        assert_eq!(store.entries.len(), 1);
    }

    #[test]
    fn apply_input_and_supervisory_keys_are_fenced_in_their_own_domains() {
        let op = phux_protocol::InputOperationId::new([5; 16]).expect("non-zero");
        let apply = Command::ApplyInput {
            operation_id: op,
            terminal_id: phux_protocol::ids::ResourceId::local(1),
            events: Vec::new(),
        };
        let kill = Command::KillResource {
            terminal_id: phux_protocol::ids::ResourceId::local(1),
            operation_id: IdempotencyKey::new([5; 16]),
        };
        let unkeyed = Command::KillResource {
            terminal_id: phux_protocol::ids::ResourceId::local(1),
            operation_id: None,
        };
        assert_ne!(fenced_key(&apply), fenced_key(&kill));
        assert!(fenced_key(&apply).is_some() && fenced_key(&kill).is_some());
        assert_eq!(fenced_key(&unkeyed), None);
    }
}
