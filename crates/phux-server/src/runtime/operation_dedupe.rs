//! The shared operation dedupe record (ADR-0053, generalized by ADR-0126).
//!
//! A client that loses a reply cannot tell whether its operation ran, so the
//! operations that must be safe to repeat carry a client-drawn 16-byte id.
//! This store binds each id to a digest of the request and, once known, to
//! the outcome, so a repeat with the same id and payload answers the original
//! outcome instead of running again, and a repeat with a different payload is
//! refused.
//!
//! One store serves every such operation, with one set of bounds: a
//! ten-minute horizon measured from admission, at most 65,536 live ids, and
//! the server incarnation as scope (it lives in memory and dies with the
//! process, which is what `HELLO_OK.server_id` tells a client). When the
//! store is full it refuses new ids rather than evicting live ones, so a
//! storm of one verb can never make another verb's retry run twice.
//!
//! Ids are namespaced by `OperationDomain`: the same 16 bytes used as an
//! `APPLY_INPUT` operation id and as a spawn key name two operations, not one.
//!
//! The record is shared between the async runtime, the input lane thread,
//! and the input completion waiter, so it is a `Mutex`, and a waiter is a
//! callback: each verb joins a pending operation with its own reply type
//! without the store knowing it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phux_protocol::ids::{ResourceId as WireResourceId, ServerInstance};
use phux_protocol::wire::frame::CommandResult;

/// Live ids the store holds at most; a new id past it is refused.
pub(crate) const DEDUPE_MAX_ENTRIES: usize = 65_536;
/// How long an id stays bound, measured from its admission.
pub(crate) const DEDUPE_RETENTION: Duration = Duration::from_mins(10);
/// Repeats that may wait on one unresolved operation at once.
pub(crate) const MAX_PENDING_RETRY_WAITERS: usize = 64;

/// Which verb an id belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum OperationDomain {
    /// `APPLY_INPUT.operation_id` (ADR-0053).
    Input,
    /// `SPAWN_RESOURCE.idempotency_key` (ADR-0126).
    Spawn,
    /// `phux.session.create/v1.request_token` (ADR-0126).
    SessionCreate,
}

/// One namespaced operation id. Debug output is redacted: an id correlates
/// the operations that reused it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct OperationKey {
    domain: OperationDomain,
    id: [u8; 16],
}

impl OperationKey {
    pub(crate) const fn new(domain: OperationDomain, id: [u8; 16]) -> Self {
        Self { domain, id }
    }
}

impl std::fmt::Debug for OperationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OperationKey({:?}, <redacted>)", self.domain)
    }
}

/// What a finished operation answers a repeat with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CachedOutcome {
    /// An `APPLY_INPUT` result.
    Input(CommandResult),
    /// A spawn's resource, and the instance token its id was allocated under.
    Spawn {
        id: WireResourceId,
        instance: ServerInstance,
    },
    /// A session create's published result document.
    SessionCreate(serde_json::Value),
}

/// A repeat parked on an unresolved operation. Called once with the outcome
/// it should answer; dropped uncalled when the operation bound nothing.
pub(crate) type Waiter = Box<dyn FnOnce(&CachedOutcome) + Send>;

/// The answer to one admission.
#[derive(Debug)]
pub(crate) enum Claim<R> {
    /// The caller runs the operation and must resolve it.
    Owner,
    /// The same operation is unresolved; `R` receives its outcome.
    Pending(R),
    /// The same operation is unresolved and has too many waiters already.
    PendingUncertain,
    /// The same operation already finished with this outcome.
    Final(CachedOutcome),
    /// The id is bound to a different payload.
    Conflict,
    /// The store is full and refuses a new id.
    Full,
}

enum EntryState {
    Pending(Vec<Waiter>),
    /// An `APPLY_INPUT` refused before it reached the writer: the id stays
    /// bound to its digest, but the next same-payload repeat runs again.
    Retryable,
    Final(CachedOutcome),
}

impl std::fmt::Debug for EntryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending(waiters) => write!(f, "Pending({} waiters)", waiters.len()),
            Self::Retryable => f.write_str("Retryable"),
            Self::Final(outcome) => f.debug_tuple("Final").field(outcome).finish(),
        }
    }
}

#[derive(Debug)]
struct Entry {
    digest: [u8; 32],
    state: EntryState,
    inserted_at: Instant,
}

#[derive(Debug, Default)]
struct DedupeStore {
    entries: HashMap<OperationKey, Entry>,
    expiry: VecDeque<(Instant, OperationKey)>,
}

impl DedupeStore {
    fn prune_expired(&mut self, now: Instant) {
        while self.expiry.front().is_some_and(|(inserted_at, _)| {
            now.saturating_duration_since(*inserted_at) >= DEDUPE_RETENTION
        }) {
            let Some((inserted_at, key)) = self.expiry.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.inserted_at == inserted_at)
            {
                self.entries.remove(&key);
            }
        }
    }

    fn is_full(&self) -> bool {
        self.entries.len() >= DEDUPE_MAX_ENTRIES
    }

    fn claim_at<R>(
        &mut self,
        key: OperationKey,
        digest: [u8; 32],
        admitted_at: Instant,
        join: impl FnOnce() -> (Waiter, R),
    ) -> Claim<R> {
        self.prune_expired(admitted_at);
        if let Some(entry) = self.entries.get_mut(&key) {
            return Self::claim_existing(entry, digest, join);
        }
        if self.is_full() {
            return Claim::Full;
        }
        self.entries.insert(
            key,
            Entry {
                digest,
                state: EntryState::Pending(Vec::new()),
                inserted_at: admitted_at,
            },
        );
        self.expiry.push_back((admitted_at, key));
        Claim::Owner
    }

    fn claim_existing<R>(
        entry: &mut Entry,
        digest: [u8; 32],
        join: impl FnOnce() -> (Waiter, R),
    ) -> Claim<R> {
        if entry.digest != digest {
            return Claim::Conflict;
        }
        match &mut entry.state {
            EntryState::Pending(waiters) => {
                if waiters.len() >= MAX_PENDING_RETRY_WAITERS {
                    return Claim::PendingUncertain;
                }
                let (waiter, receiver) = join();
                waiters.push(waiter);
                Claim::Pending(receiver)
            }
            EntryState::Retryable => {
                entry.state = EntryState::Pending(Vec::new());
                Claim::Owner
            }
            EntryState::Final(outcome) => Claim::Final(outcome.clone()),
        }
    }

    fn set_final(&mut self, key: OperationKey, outcome: CachedOutcome) -> Vec<Waiter> {
        let Some(entry) = self.entries.get_mut(&key) else {
            return Vec::new();
        };
        match std::mem::replace(&mut entry.state, EntryState::Final(outcome)) {
            EntryState::Pending(waiters) => waiters,
            EntryState::Retryable | EntryState::Final(_) => Vec::new(),
        }
    }

    fn set_retryable(&mut self, key: OperationKey) -> Vec<Waiter> {
        let Some(entry) = self.entries.get_mut(&key) else {
            return Vec::new();
        };
        match std::mem::replace(&mut entry.state, EntryState::Retryable) {
            EntryState::Pending(waiters) => waiters,
            EntryState::Retryable | EntryState::Final(_) => Vec::new(),
        }
    }

    /// Forget an unresolved operation that bound nothing, returning its
    /// waiters. A finished one is kept.
    fn release(&mut self, key: OperationKey) -> Vec<Waiter> {
        let Some(entry) = self.entries.get(&key) else {
            return Vec::new();
        };
        if !matches!(entry.state, EntryState::Pending(_)) {
            return Vec::new();
        }
        let waiters = match self.entries.remove(&key).map(|entry| entry.state) {
            Some(EntryState::Pending(waiters)) => waiters,
            _ => Vec::new(),
        };
        self.compact_expiry();
        waiters
    }

    /// Forget a finished operation, but only while it still records
    /// `outcome`: the resource it answered with was withdrawn before its
    /// creator could use it.
    fn unbind(&mut self, key: OperationKey, outcome: &CachedOutcome) {
        let bound = self.entries.get(&key).is_some_and(
            |entry| matches!(&entry.state, EntryState::Final(held) if held == outcome),
        );
        if bound {
            self.entries.remove(&key);
            self.compact_expiry();
        }
    }

    /// A released id leaves its expiry slot behind. Drop the stale slots
    /// once they outnumber the bound, so refused operations cannot grow the
    /// queue without limit inside one horizon.
    fn compact_expiry(&mut self) {
        if self.expiry.len() <= DEDUPE_MAX_ENTRIES.saturating_mul(2) {
            return;
        }
        let entries = &self.entries;
        self.expiry.retain(|(inserted_at, key)| {
            entries
                .get(key)
                .is_some_and(|entry| entry.inserted_at == *inserted_at)
        });
    }
}

/// The server's one dedupe record. Cloning shares it.
#[derive(Clone, Debug, Default)]
pub(crate) struct OperationDedupe(Arc<Mutex<DedupeStore>>);

impl OperationDedupe {
    fn lock(&self) -> std::sync::MutexGuard<'_, DedupeStore> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Admit `key` with `digest` at `admitted_at`. `join` builds the waiter
    /// and its receiving half, and runs only when the caller joins an
    /// unresolved operation.
    pub(crate) fn claim_at<R>(
        &self,
        key: OperationKey,
        digest: [u8; 32],
        admitted_at: Instant,
        join: impl FnOnce() -> (Waiter, R),
    ) -> Claim<R> {
        self.lock().claim_at(key, digest, admitted_at, join)
    }

    /// Record the outcome a repeat answers, and hand it to the waiters.
    pub(crate) fn set_final(&self, key: OperationKey, outcome: &CachedOutcome) {
        let waiters = self.lock().set_final(key, outcome.clone());
        notify(waiters, outcome);
    }

    /// Keep the id-to-digest binding but not `notice`, which only the
    /// current waiters receive.
    pub(crate) fn set_retryable(&self, key: OperationKey, notice: &CachedOutcome) {
        let waiters = self.lock().set_retryable(key);
        notify(waiters, notice);
    }

    /// Forget an unresolved operation that bound nothing. Its waiters are
    /// dropped uncalled, so each may admit itself again.
    pub(crate) fn release(&self, key: OperationKey) {
        let waiters = self.lock().release(key);
        drop(waiters);
    }

    /// Forget a finished operation whose recorded outcome is still
    /// `outcome`, so its next repeat runs again. A different outcome is kept.
    pub(crate) fn unbind(&self, key: OperationKey, outcome: &CachedOutcome) {
        self.lock().unbind(key, outcome);
    }
}

fn notify(waiters: Vec<Waiter>, outcome: &CachedOutcome) {
    for waiter in waiters {
        waiter(outcome);
    }
}

/// Ownership of one admitted operation. Dropping it releases the id unless
/// the operation recorded an outcome first, so an operation that failed, or
/// whose task was cancelled, binds nothing.
#[derive(Debug)]
pub(crate) struct OperationClaim {
    dedupe: OperationDedupe,
    key: OperationKey,
}

impl OperationClaim {
    pub(crate) const fn new(dedupe: OperationDedupe, key: OperationKey) -> Self {
        Self { dedupe, key }
    }

    /// Record the outcome this operation's repeats answer.
    pub(crate) fn bind(&self, outcome: &CachedOutcome) {
        self.dedupe.set_final(self.key, outcome);
    }
}

impl Drop for OperationClaim {
    fn drop(&mut self) {
        self.dedupe.release(self.key);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn input_key(value: u64) -> OperationKey {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        OperationKey::new(OperationDomain::Input, bytes)
    }

    fn no_join() -> (Waiter, ()) {
        (Box::new(|_| {}), ())
    }

    fn channel_join() -> (Waiter, tokio::sync::oneshot::Receiver<CachedOutcome>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            Box::new(move |outcome: &CachedOutcome| {
                let _ = tx.send(outcome.clone());
            }),
            rx,
        )
    }

    fn spawned(id: u32) -> CachedOutcome {
        CachedOutcome::Spawn {
            id: WireResourceId::local(id),
            instance: ServerInstance::new([9; 16]),
        }
    }

    #[test]
    fn operation_cache_prunes_only_expired_prefix_and_treats_over_capacity_as_full() {
        let start = Instant::now();
        let first = input_key(1);
        let second = input_key(2);
        let mut cache = DedupeStore::default();
        assert!(matches!(
            cache.claim_at(first, [1; 32], start, no_join),
            Claim::Owner
        ));
        drop(cache.set_final(first, CachedOutcome::Input(CommandResult::Ok)));
        assert!(matches!(
            cache.claim_at(second, [2; 32], start + Duration::from_secs(1), no_join),
            Claim::Owner
        ));
        drop(cache.set_final(second, CachedOutcome::Input(CommandResult::Ok)));
        cache.prune_expired(start + DEDUPE_RETENTION);
        assert!(!cache.entries.contains_key(&first));
        assert!(cache.entries.contains_key(&second));
        assert_eq!(cache.expiry.len(), 1);

        for value in 3..=u64::try_from(DEDUPE_MAX_ENTRIES + 1).unwrap() {
            assert!(matches!(
                cache.claim_at(
                    input_key(value),
                    [0; 32],
                    start + Duration::from_secs(1),
                    no_join,
                ),
                Claim::Owner
            ));
        }
        assert_eq!(cache.entries.len(), DEDUPE_MAX_ENTRIES);
        assert!(cache.is_full(), "capacity check must use >=");
        assert!(matches!(
            cache.claim_at(
                input_key(u64::try_from(DEDUPE_MAX_ENTRIES + 2).unwrap()),
                [0; 32],
                start + Duration::from_secs(1),
                no_join,
            ),
            Claim::Full
        ));
        assert_eq!(cache.entries.len(), DEDUPE_MAX_ENTRIES);
    }

    #[test]
    fn cache_uses_admission_time_for_retry_at_expiry_boundary() {
        let start = Instant::now();
        let key = input_key(18);
        let digest = [0x18; 32];
        let mut cache = DedupeStore::default();
        assert!(matches!(
            cache.claim_at(key, digest, start, no_join),
            Claim::Owner
        ));
        drop(cache.set_final(key, CachedOutcome::Input(CommandResult::Ok)));

        let admitted_before_expiry = start
            + DEDUPE_RETENTION
                .checked_sub(Duration::from_nanos(1))
                .expect("retention exceeds one nanosecond");
        let processed_after_expiry = start + DEDUPE_RETENTION + Duration::from_secs(1);
        assert!(processed_after_expiry > start + DEDUPE_RETENTION);
        let binding = cache.claim_at(key, digest, admitted_before_expiry, no_join);
        cache.prune_expired(processed_after_expiry);
        assert!(matches!(
            binding,
            Claim::Final(CachedOutcome::Input(CommandResult::Ok))
        ));
        assert!(cache.expiry.is_empty());
    }

    #[test]
    fn pending_retry_waiters_are_bounded_with_an_uncertain_outcome() {
        let key = input_key(20);
        let digest = [0x20; 32];
        let mut cache = DedupeStore::default();
        assert!(matches!(
            cache.claim_at(key, digest, Instant::now(), no_join),
            Claim::Owner
        ));
        let mut waiters = Vec::new();
        for _ in 0..MAX_PENDING_RETRY_WAITERS {
            let Claim::Pending(waiter) = cache.claim_at(key, digest, Instant::now(), channel_join)
            else {
                panic!("pending retry should join within the bound");
            };
            waiters.push(waiter);
        }
        assert!(matches!(
            cache.claim_at(key, digest, Instant::now(), channel_join),
            Claim::PendingUncertain
        ));
        assert_eq!(waiters.len(), MAX_PENDING_RETRY_WAITERS);
    }

    /// ADR-0126: a keyed spawn repeated past the horizon is a new operation.
    #[test]
    fn keyed_spawn_after_the_horizon_spawns_again() {
        let start = Instant::now();
        let key = OperationKey::new(OperationDomain::Spawn, [5; 16]);
        let mut cache = DedupeStore::default();
        assert!(matches!(
            cache.claim_at(key, [5; 32], start, no_join),
            Claim::Owner
        ));
        drop(cache.set_final(key, spawned(7)));
        let inside = start
            + DEDUPE_RETENTION
                .checked_sub(Duration::from_secs(1))
                .expect("retention exceeds one second");
        assert!(matches!(
            cache.claim_at(key, [5; 32], inside, no_join),
            Claim::Final(outcome) if outcome == spawned(7)
        ));
        assert!(matches!(
            cache.claim_at(key, [6; 32], inside, no_join),
            Claim::Conflict
        ));
        assert!(
            matches!(
                cache.claim_at(key, [6; 32], start + DEDUPE_RETENTION, no_join),
                Claim::Owner
            ),
            "past the horizon the key is unknown, whatever the payload"
        );
    }

    /// The same bytes in two domains are two operations.
    #[test]
    fn domains_do_not_share_ids() {
        let mut cache = DedupeStore::default();
        let now = Instant::now();
        let input = OperationKey::new(OperationDomain::Input, [1; 16]);
        let spawn = OperationKey::new(OperationDomain::Spawn, [1; 16]);
        assert!(matches!(
            cache.claim_at(input, [1; 32], now, no_join),
            Claim::Owner
        ));
        assert!(matches!(
            cache.claim_at(spawn, [2; 32], now, no_join),
            Claim::Owner
        ));
    }

    /// A released operation bound nothing: its waiters see the channel
    /// close, and the next repeat owns the id again, under any payload.
    #[test]
    fn release_forgets_an_unresolved_operation_and_keeps_a_finished_one() {
        let dedupe = OperationDedupe::default();
        let now = Instant::now();
        let key = OperationKey::new(OperationDomain::Spawn, [3; 16]);
        assert!(matches!(
            dedupe.claim_at(key, [3; 32], now, no_join),
            Claim::Owner
        ));
        let Claim::Pending(mut waiter) = dedupe.claim_at(key, [3; 32], now, channel_join) else {
            panic!("the repeat joins the unresolved spawn");
        };
        drop(OperationClaim::new(dedupe.clone(), key));
        assert!(
            waiter.try_recv().is_err(),
            "a released waiter hears nothing"
        );
        assert!(matches!(
            dedupe.claim_at(key, [4; 32], now, no_join),
            Claim::Owner
        ));

        let claim = OperationClaim::new(dedupe.clone(), key);
        let Claim::Pending(mut waiter) = dedupe.claim_at(key, [4; 32], now, channel_join) else {
            panic!("the repeat joins the unresolved spawn");
        };
        claim.bind(&spawned(8));
        drop(claim);
        assert_eq!(waiter.try_recv().expect("bound outcome"), spawned(8));
        assert!(matches!(
            dedupe.claim_at(key, [4; 32], now, no_join),
            Claim::Final(outcome) if outcome == spawned(8)
        ));
    }

    #[test]
    fn released_expiry_slots_are_compacted_past_the_bound() {
        let mut cache = DedupeStore::default();
        let now = Instant::now();
        for value in 0..=u64::try_from(DEDUPE_MAX_ENTRIES * 2).unwrap() {
            let key = input_key(value + 1);
            assert!(matches!(
                cache.claim_at(key, [0; 32], now, no_join),
                Claim::Owner
            ));
            drop(cache.release(key));
        }
        assert!(cache.entries.is_empty());
        assert!(cache.expiry.len() <= DEDUPE_MAX_ENTRIES * 2);
    }

    /// A withdrawn resource takes its binding with it, and only its own.
    #[test]
    fn unbind_forgets_only_the_outcome_it_names() {
        let dedupe = OperationDedupe::default();
        let now = Instant::now();
        let key = OperationKey::new(OperationDomain::Spawn, [7; 16]);
        assert!(matches!(
            dedupe.claim_at(key, [7; 32], now, no_join),
            Claim::Owner
        ));
        dedupe.set_final(key, &spawned(9));
        dedupe.unbind(key, &spawned(10));
        assert!(
            matches!(dedupe.claim_at(key, [7; 32], now, no_join), Claim::Final(_)),
            "another outcome leaves the binding alone"
        );
        dedupe.unbind(key, &spawned(9));
        assert!(
            matches!(dedupe.claim_at(key, [8; 32], now, no_join), Claim::Owner),
            "an unbound key is new again, under any payload"
        );
    }
}
