//! Unit tests for the live revocation judgement (`workload-auth.md` §7).
//! The wire-level conformance sweep lives in `runtime::revocation_conformance`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use phux_protocol::scope::{EffectiveScopeSet, TerminalScopeSet};

use super::{contains_clause, covers, earliest};
use phux_protocol::scope::Selector;

fn set(scopes: &[&str]) -> TerminalScopeSet {
    TerminalScopeSet::parse_all(scopes).unwrap()
}

/// Whether every clause `minted` produced is still inside `current`.
fn still_contained(minted: &[&str], current: &[&str]) -> bool {
    let effective = EffectiveScopeSet::unattenuated(&set(minted)).unwrap();
    let ceiling = set(current);
    effective
        .clauses()
        .iter()
        .all(|clause| contains_clause(&ceiling, clause))
}

#[test]
fn an_unchanged_or_wider_ceiling_still_contains_the_minted_clauses() {
    assert!(still_contained(&["observe@global"], &["observe@global"]));
    assert!(still_contained(
        &["observe@global"],
        &["observe,input@global"]
    ));
    assert!(still_contained(&["observe@host"], &["observe@global"]));
    assert!(still_contained(
        &["observe@host:sat", "input@host"],
        &["observe,input@global"]
    ));
}

#[test]
fn a_lost_verb_or_a_narrower_selector_no_longer_contains_them() {
    assert!(!still_contained(
        &["observe,input@global"],
        &["observe@global"]
    ));
    assert!(!still_contained(&["observe@global"], &["observe@host"]));
    assert!(!still_contained(
        &["observe@host:sat"],
        &["observe@host:other"]
    ));
    assert!(!still_contained(&["observe@global"], &[]));
}

#[test]
fn a_group_never_statically_covers_a_terminal() {
    assert!(!covers(&Selector::Group(1), &Selector::TerminalLocal(7)));
    assert!(covers(&Selector::HostLocal, &Selector::TerminalLocal(7)));
    assert!(covers(&Selector::HostLocal, &Selector::Group(1)));
    assert!(!covers(&Selector::HostLocal, &Selector::Global));
}

/// §7 step 1: a revoked grant admits nothing, whatever its shape. The
/// owner-shaped grant a bearer holds in the transitional posture is the case
/// the owner shortcut would otherwise let through.
#[test]
fn a_revoked_grant_admits_nothing_even_owner_shaped() {
    use crate::policy::{ConnectionGrant, Request, Revocation, enforce};
    use phux_protocol::wire::frame::FrameKind;

    let state = crate::state::ServerState::new();
    let client = crate::state::ClientId(1);
    let ping = FrameKind::Ping { nonce: 1 };
    let mut owner = ConnectionGrant::owner();
    assert!(enforce(&state, client, &owner, Request::Frame(&ping)).is_ok());
    owner.revoke(Revocation::Revoked);
    assert!(enforce(&state, client, &owner, Request::Frame(&ping)).is_err());

    let mut scoped =
        ConnectionGrant::scoped(TerminalScopeSet::all_global(), Some("id".to_owned())).unwrap();
    scoped.revoke(Revocation::Expired);
    scoped.revoke(Revocation::Revoked);
    assert_eq!(
        scoped.revocation(),
        Some(Revocation::Expired),
        "the first cause sticks"
    );
    assert!(enforce(&state, client, &scoped, Request::Frame(&ping)).is_err());
}

#[test]
fn earliest_ignores_absent_deadlines() {
    let now = chrono::Utc::now();
    let later = now + chrono::Duration::seconds(5);
    assert_eq!(earliest(None, None), None);
    assert_eq!(earliest(Some(later), None), Some(later));
    assert_eq!(earliest(Some(later), Some(now)), Some(now));
}
