//! The hub's incarnation fence and actor correlation for keyed operations it
//! forwards to one satellite (`docs/spec/L1.md` §9.1).
//!
//! A satellite's dedupe record dies with its process (a changed
//! `HELLO_OK.server_id`), which a consumer behind the hub cannot see. The
//! hub records the incarnation each operation id was forwarded to and
//! answers a retry across a restart with `INCARNATION_CHANGED`. The record
//! also names the consumer that sent the operation and the satellite
//! terminals it targets, for re-stamped events: a satellite may attribute an
//! event to a forwarded operation only on a terminal that operation named.
//! Bounded like the dedupe record (ten-minute horizon, entry cap; full
//! refuses rather than evicts) and shared across the link's reconnects.

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
    /// The satellite-local terminals the operation targets.
    terminals: Vec<u32>,
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
        terminals: Vec<u32>,
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
            entry.terminals = terminals;
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
                terminals,
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

    /// Admit forwarding the operation `key` from hub consumer `actor`,
    /// targeting satellite `terminals`, to the satellite incarnation the
    /// link is connected to.
    pub(crate) fn admit(
        &self,
        key: OperationKey,
        incarnation: Incarnation,
        actor: ClientId,
        terminals: Vec<u32>,
        now: Instant,
    ) -> FenceVerdict {
        self.lock().admit(key, incarnation, actor, terminals, now)
    }

    /// What the `operation_id` a satellite stamped on an event for its
    /// terminal `terminal` means to the hub, while the record holds it.
    pub(crate) fn event_operation(
        &self,
        operation_id: &IdempotencyKey,
        terminal: u32,
    ) -> EventOperation {
        let store = self.lock();
        let forwarded = [OperationDomain::Signal, OperationDomain::Input]
            .into_iter()
            .find_map(|domain| {
                store
                    .entries
                    .get(&OperationKey::new(domain, *operation_id.as_bytes()))
            });
        match forwarded {
            None => EventOperation::Unknown,
            Some(entry) if entry.terminals.contains(&terminal) => {
                EventOperation::Forwarded(entry.actor)
            }
            Some(_) => EventOperation::Misattributed,
        }
    }
}

/// How the hub treats the `operation_id` a satellite stamped on an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventOperation {
    /// A keyed operation a hub consumer forwarded for this terminal: the
    /// event is that consumer's.
    Forwarded(ClientId),
    /// An id this link never forwarded (one of the satellite's own
    /// clients, or a spawn key): kept, attributed to no hub consumer.
    Unknown,
    /// An id a hub consumer forwarded for another terminal: the satellite
    /// may not claim it here, so the hub drops it from the event.
    Misattributed,
}

/// The satellite-local terminals a fenced command targets.
pub(crate) fn fenced_terminals(command: &Command) -> Vec<u32> {
    let ids: Vec<&phux_protocol::ids::ResourceId> = match command {
        Command::ApplyInput { terminal_id, .. }
        | Command::KillResource { terminal_id, .. }
        | Command::KillResourceIf { terminal_id, .. }
        | Command::SignalTerminal { terminal_id, .. } => vec![terminal_id],
        Command::KillResources { ids, .. } => ids.iter().collect(),
        _ => Vec::new(),
    };
    ids.into_iter()
        .filter_map(phux_protocol::ids::ResourceId::local_id)
        .collect()
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
            fence.admit(key(1), A, ClientId(7), vec![1], now),
            FenceVerdict::Forward
        );
        assert_eq!(
            fence.admit(key(1), A, ClientId(8), vec![1], now),
            FenceVerdict::Forward,
            "a retry to the same satellite process is the satellite's to dedupe"
        );
        assert_eq!(
            fence.admit(key(1), B, ClientId(8), vec![1], now),
            FenceVerdict::IncarnationChanged
        );
        assert_eq!(
            fence.admit(key(2), B, ClientId(8), vec![1], now),
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
        assert_eq!(fence.event_operation(&id, 5), EventOperation::Unknown);
        fence.admit(key, A, ClientId(3), vec![5], now);
        assert_eq!(
            fence.event_operation(&id, 5),
            EventOperation::Forwarded(ClientId(3))
        );
        fence.admit(key, A, ClientId(4), vec![5], now);
        assert_eq!(
            fence.event_operation(&id, 5),
            EventOperation::Forwarded(ClientId(4))
        );
    }

    /// A satellite cannot attribute an event on one of its terminals to an
    /// operation a hub consumer sent for another: the id is misattributed,
    /// in either dedupe domain.
    #[test]
    fn a_forwarded_id_names_only_the_terminals_it_targeted() {
        let fence = OperationFence::default();
        let now = Instant::now();
        let signal = Command::SignalTerminal {
            terminal_id: phux_protocol::ids::ResourceId::local(5),
            signal: phux_protocol::wire::frame::TerminalSignal::Interrupt,
            operation_id: IdempotencyKey::new([1; 16]),
        };
        let batch = Command::KillResources {
            ids: vec![
                phux_protocol::ids::ResourceId::local(6),
                phux_protocol::ids::ResourceId::local(7),
            ],
            operation_id: IdempotencyKey::new([2; 16]),
        };
        let apply = Command::ApplyInput {
            operation_id: phux_protocol::InputOperationId::new([3; 16]).expect("non-zero"),
            terminal_id: phux_protocol::ids::ResourceId::local(8),
            events: Vec::new(),
        };
        for command in [&signal, &batch, &apply] {
            let key = fenced_key(command).expect("keyed");
            fence.admit(key, A, ClientId(9), fenced_terminals(command), now);
        }
        let id = |byte: u8| IdempotencyKey::new([byte; 16]).expect("non-zero");
        assert_eq!(
            fence.event_operation(&id(1), 5),
            EventOperation::Forwarded(ClientId(9))
        );
        assert_eq!(
            fence.event_operation(&id(1), 6),
            EventOperation::Misattributed
        );
        assert_eq!(
            fence.event_operation(&id(2), 7),
            EventOperation::Forwarded(ClientId(9))
        );
        assert_eq!(
            fence.event_operation(&id(2), 5),
            EventOperation::Misattributed
        );
        assert_eq!(
            fence.event_operation(&id(3), 5),
            EventOperation::Misattributed
        );
        assert_eq!(
            fence.event_operation(&id(3), 8),
            EventOperation::Forwarded(ClientId(9))
        );
    }

    #[test]
    fn the_fence_is_bounded_and_shares_the_dedupe_horizon() {
        let mut store = FenceStore::default();
        let start = Instant::now();
        for value in 0..u64::try_from(DEDUPE_MAX_ENTRIES).expect("fits") {
            assert_eq!(
                store.admit(key(value), A, ClientId(1), Vec::new(), start),
                FenceVerdict::Forward
            );
        }
        let extra = u64::try_from(DEDUPE_MAX_ENTRIES).expect("fits");
        assert_eq!(
            store.admit(key(extra), A, ClientId(1), Vec::new(), start),
            FenceVerdict::Full,
            "a full fence refuses a new id instead of evicting a live one"
        );
        let inside = start
            + DEDUPE_RETENTION
                .checked_sub(Duration::from_secs(1))
                .expect("retention exceeds one second");
        assert_eq!(
            store.admit(key(0), B, ClientId(1), Vec::new(), inside),
            FenceVerdict::IncarnationChanged,
            "inside the horizon the old incarnation still fences"
        );
        let past = start + DEDUPE_RETENTION;
        assert_eq!(
            store.admit(key(0), B, ClientId(1), Vec::new(), past),
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
