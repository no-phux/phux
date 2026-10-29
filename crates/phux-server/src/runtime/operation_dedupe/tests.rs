use super::*;

fn input_key(value: u64) -> OperationKey {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&value.to_be_bytes());
    OperationKey::new(OperationDomain::Input, bytes)
}

use super::{join_nothing as no_join, join_outcome as channel_join};

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
