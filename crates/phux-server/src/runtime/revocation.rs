//! Live revocation (`docs/spec/workload-auth.md` §7, ADR-0116).
//!
//! A connection's authority is not a snapshot of its HELLO. The `Watcher`
//! here re-judges every live connection whose authority someone else can
//! withdraw (a scoped workload grant, or any grant a pairing-store bearer
//! admitted) whenever the workload registry or a token store changes, and
//! at each grant's expiry. When the authority is gone, `revoke_connection`
//! performs §7's steps in one critical section of the state lock:
//!
//! 1. the grant is marked revoked, so every guard denies everything the
//!    connection sends from then on;
//! 2. its input leases (each journaled as `terminal_control { RELEASED }`
//!    with no actor), subscriptions, relay proxies, and queued input are
//!    released;
//! 3. its writer is told to drop everything queued and send
//!    `ERROR { PERMISSION_DENIED }` and `DETACHED { AUTHORIZATION_REVOKED |
//!    AUTHORIZATION_EXPIRED }` (the writer owns the goodbye, so a mailbox
//!    that is full, or a read loop parked on one, cannot delay it); and
//! 4. the connection token is cancelled, which closes the transport and
//!    aborts its bulk work and pending input receipts.
//!
//! Revocation never kills the connection's Terminals or processes.
//!
//! The owner socket's grant is never watched: only the kernel uid stands
//! behind it. With no scoped and no bearer-admitted connection, the watcher
//! parks on its wake handle and does nothing.
//!
//! What counts as a verdict differs by source, because a revocation cut is
//! final for a phone (it treats the refusal as fatal):
//!
//! - A pairing-token store gives a verdict only when it loaded cleanly and
//!   holds credentials: the admitting generation is revoked, expired, or
//!   absent from it. A missing, empty, insecure, unparseable, or unreadable
//!   store is never a verdict for live sessions; it only refuses new ones.
//! - The workload registry's verdicts (a revoked, expired, or removed
//!   credential, or a narrowed ceiling) apply at once. A missing, insecure,
//!   or malformed registry applies §7's empty snapshot only after the same
//!   broken state has persisted for `REGISTRY_GRACE` (five seconds), so a write caught
//!   mid-way cannot revoke every workload. A transient read is no verdict.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use phux_protocol::kinds::Verbs;
use phux_protocol::scope::{EffectiveClause, Selector, TerminalScopeSet};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::auth::{BearerAdmission, ReloadingTokenStore, Standing, TokenStore};
use crate::policy::{
    Authority, ConnectionGrant, POLICY_TARGET, RegistryStamp, Revocation, WatchedConnection,
};
use crate::state::{ClientId, ServerState, SharedState};
use crate::workload::{BrokenRegistry, RegistryObservation, WorkloadCredential, WorkloadRegistry};

#[cfg(test)]
mod tests;

/// How often the watcher re-reads the registry and the token stores while a
/// watched connection is live. Each poll costs one `stat` per store; a file
/// is re-read only when it changed.
pub(crate) const POLL: Duration = Duration::from_millis(250);

/// How long the workload registry must stay in one broken state (missing,
/// insecure, or malformed) before its empty snapshot revokes the workload
/// connections it stood behind: twenty polls.
pub(crate) const REGISTRY_GRACE: Duration = Duration::from_secs(5);

/// Spawn the watcher on the current `LocalSet`. It runs until `root` fires.
pub(crate) fn spawn_revocation_watcher(state: &SharedState, root: &CancellationToken) {
    tokio::task::spawn_local(watch(state.clone(), root.clone()));
}

async fn watch(state: SharedState, root: CancellationToken) {
    let wake = state.with(ServerState::revocation_wake);
    let mut watcher = Watcher::new();
    loop {
        let pause = watcher.sweep(&state);
        let rest = async move {
            match pause {
                Some(pause) => tokio::time::sleep(pause).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = root.cancelled() => return,
            () = wake.notified() => {}
            () = rest => {}
        }
    }
}

/// The watcher's memory across sweeps.
pub(crate) struct Watcher {
    /// How long a broken registry state must persist before it revokes.
    grace: Duration,
    /// The broken registry state first seen, and when.
    broken_since: Option<(BrokenRegistry, Instant)>,
    /// The registry snapshot every watched grant was last judged against.
    confirmed: Option<Arc<WorkloadRegistry>>,
}

impl Watcher {
    /// A watcher with the production grace window.
    pub(crate) const fn new() -> Self {
        Self::with_grace(REGISTRY_GRACE)
    }

    /// A watcher whose broken-registry grace window is `grace`.
    pub(crate) const fn with_grace(grace: Duration) -> Self {
        Self {
            grace,
            broken_since: None,
            confirmed: None,
        }
    }

    /// Re-judge every watched connection now and revoke each whose
    /// authority is gone. Returns how long until the next judgement is due,
    /// or `None` when nothing is watched.
    pub(crate) fn sweep(&mut self, state: &SharedState) -> Option<Duration> {
        let watched = state.with(ServerState::watched_connections);
        if watched.is_empty() {
            return None;
        }
        let registry = self.observe(state);
        let mut stores = BearerStores::default();
        let now = Utc::now();
        let mut next_expiry = None;
        for connection in &watched {
            match judge(connection, &registry, &mut stores, now) {
                Err(revocation) => revoke_connection(state, connection.client, revocation),
                Ok(kept) => {
                    if let Some(stamp) = kept.refresh {
                        state.with_mut(|s| s.refresh_connection_grant(connection.client, stamp));
                    }
                    next_expiry = earliest(next_expiry, kept.expires_at);
                }
            }
        }
        if let RegistryView::Snapshot { registry, .. } = registry {
            self.confirmed = Some(registry);
        }
        Some(next_expiry.map_or(POLL, |expiry| until(expiry, now).min(POLL)))
    }

    /// Observe the registry the state's engine mints from. Reads the file,
    /// so it runs off the state lock.
    fn observe(&mut self, state: &SharedState) -> RegistryView {
        let Some(registry) = state.with(|s| s.policy_engine().workload_registry()) else {
            self.broken_since = None;
            return RegistryView::Absent;
        };
        match registry.observe() {
            RegistryObservation::Loaded(snapshot) => {
                self.broken_since = None;
                let confirmed = self
                    .confirmed
                    .as_ref()
                    .is_some_and(|last| Arc::ptr_eq(last, &snapshot));
                RegistryView::Snapshot {
                    registry: snapshot,
                    confirmed,
                }
            }
            RegistryObservation::Broken(broken) => self.broken(broken),
            RegistryObservation::Transient => RegistryView::Unreadable,
        }
    }

    /// A broken registry is no verdict until the same broken state has
    /// persisted for the grace window; then its empty snapshot applies.
    fn broken(&mut self, broken: BrokenRegistry) -> RegistryView {
        let now = Instant::now();
        let since = match self.broken_since {
            Some((seen, since)) if seen == broken => since,
            _ => {
                warn!(target: POLICY_TARGET, grace = ?self.grace, "workload registry is missing, insecure, or malformed; its workload connections end if it stays so");
                self.broken_since = Some((broken, now));
                now
            }
        };
        if now.saturating_duration_since(since) < self.grace {
            return RegistryView::Unreadable;
        }
        RegistryView::Broken
    }
}

/// What the watcher learned from the engine's workload registry this sweep.
enum RegistryView {
    /// The engine reads no registry: only expiry ends its grants.
    Absent,
    /// No verdict this time: a transient read, or a broken state still
    /// inside its grace window.
    Unreadable,
    /// A broken state past its grace window: the empty snapshot applies.
    Broken,
    /// A valid registry; `confirmed` when it is the very snapshot every
    /// watched grant was last judged against.
    Snapshot {
        registry: Arc<WorkloadRegistry>,
        confirmed: bool,
    },
}

/// Each token store's verdict-bearing snapshot, read once per sweep however
/// many connections it admitted.
#[derive(Default)]
struct BearerStores(Vec<(Arc<ReloadingTokenStore>, Option<TokenStore>)>);

impl BearerStores {
    fn standing(&mut self, bearer: &BearerAdmission, now: DateTime<Utc>) -> Option<Standing> {
        self.snapshot_of(bearer.store())
            .map(|store| store.standing_at(&bearer.id, bearer.generation, now))
    }

    /// `store`'s verdict-bearing snapshot for this sweep, read on first use.
    fn snapshot_of(&mut self, store: &Arc<ReloadingTokenStore>) -> Option<&TokenStore> {
        let index =
            if let Some(index) = self.0.iter().position(|(seen, _)| Arc::ptr_eq(seen, store)) {
                index
            } else {
                self.0.push((Arc::clone(store), store.watch_snapshot()));
                self.0.len() - 1
            };
        self.0[index].1.as_ref()
    }
}

/// A connection whose authority still stands.
struct Kept {
    /// When it is next due to be judged for expiry.
    expires_at: Option<DateTime<Utc>>,
    /// A newer registry state that still contains its grant.
    refresh: Option<RegistryStamp>,
}

impl Kept {
    const fn unchanged(expires_at: Option<DateTime<Utc>>) -> Self {
        Self {
            expires_at,
            refresh: None,
        }
    }
}

/// Withdraw `client`'s authority while it is live (`workload-auth.md` §7).
/// Idempotent: a connection already revoked is left alone.
pub(crate) fn revoke_connection(state: &SharedState, client: ClientId, revocation: Revocation) {
    let mut revoked_now = false;
    let detached_from = state.with_mut(|s| {
        if s.connection_revoked(client) {
            return None;
        }
        revoked_now = true;
        s.mark_connection_revoked(client, revocation);
        let detached_from = super::client::release_revoked_consumer_state(s, client);
        if let Some(token) = s.client_connection_cancellation(client) {
            token.cancel();
        }
        detached_from
    });
    if !revoked_now {
        return;
    }
    info!(target: POLICY_TARGET, ?client, ?revocation, "authority withdrawn; closing the connection");
    super::client::fire_client_detached(state, client, detached_from);
}

/// Whether `connection`'s authority still stands at `now`, and until when.
fn judge(
    connection: &WatchedConnection,
    registry: &RegistryView,
    stores: &mut BearerStores,
    now: DateTime<Utc>,
) -> Result<Kept, Revocation> {
    let bearer_expiry = match &connection.bearer {
        Some(bearer) => judge_bearer(stores.standing(bearer, now))?,
        None => None,
    };
    let mut kept = judge_grant(&connection.grant, registry, now)?;
    kept.expires_at = earliest(kept.expires_at, bearer_expiry);
    Ok(kept)
}

/// A bearer that a clean, non-empty store shows revoked, removed, or
/// expired ends the connection. No verdict keeps it.
const fn judge_bearer(standing: Option<Standing>) -> Result<Option<DateTime<Utc>>, Revocation> {
    match standing {
        Some(Standing::Revoked) => Err(Revocation::Revoked),
        Some(Standing::Expired) => Err(Revocation::Expired),
        Some(Standing::Active { expires_at }) => Ok(expires_at),
        None => Ok(None),
    }
}

/// A scoped grant ends at its expiry, and when its credential's current
/// registry record no longer contains every clause it minted. The owner's
/// grant has neither.
fn judge_grant(
    grant: &ConnectionGrant,
    registry: &RegistryView,
    now: DateTime<Utc>,
) -> Result<Kept, Revocation> {
    let Authority::Scoped { effective, .. } = &grant.authority else {
        return Ok(Kept::unchanged(None));
    };
    if grant.is_expired_at(now) {
        return Err(Revocation::Expired);
    }
    let unchanged = Kept::unchanged(grant.expires_at);
    let Some(id) = grant.credential_id.as_deref() else {
        return Ok(unchanged);
    };
    let (snapshot, confirmed) = match registry {
        RegistryView::Absent | RegistryView::Unreadable => return Ok(unchanged),
        RegistryView::Broken => return Err(Revocation::Revoked),
        RegistryView::Snapshot {
            registry,
            confirmed,
        } => (registry, *confirmed),
    };
    if confirmed && confirmed_by(grant, snapshot) {
        return Ok(unchanged);
    }
    let record = snapshot
        .credentials()
        .iter()
        .find(|record| record.id == id)
        .ok_or(Revocation::Revoked)?;
    let expires_at = judge_record(record, effective.clauses(), now)?;
    Ok(Kept {
        expires_at,
        refresh: Some(RegistryStamp {
            instance: snapshot.instance_id().map(str::to_owned),
            generation: snapshot.generation(),
            expires_at,
        }),
    })
}

/// Whether `snapshot` is the registry state that minted or last confirmed
/// `grant`. Keyed on the pair `(instance, generation)`, never the generation
/// alone, and a registry with no instance id confirms nothing (§2). The
/// caller also requires the very snapshot the grant was last judged
/// against, so a hand edit that keeps the generation is still re-judged.
fn confirmed_by(grant: &ConnectionGrant, snapshot: &WorkloadRegistry) -> bool {
    snapshot.instance_id().is_some_and(|instance| {
        grant.registry_instance.as_deref() == Some(instance)
            && grant.registry_generation == snapshot.generation()
    })
}

/// Judge a credential's current record against the clauses a grant minted
/// from it. Returns the record's expiry when the grant still stands.
fn judge_record(
    record: &WorkloadCredential,
    clauses: &[EffectiveClause],
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, Revocation> {
    if record.revoked_at.is_some() {
        return Err(Revocation::Revoked);
    }
    let expires_at = record
        .expires_at
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
    if expires_at.is_some_and(|expiry| expiry <= now) {
        return Err(Revocation::Expired);
    }
    let ceiling = record.ceiling().map_err(|_| Revocation::Revoked)?;
    if !clauses
        .iter()
        .all(|clause| contains_clause(&ceiling, clause))
    {
        return Err(Revocation::Revoked);
    }
    Ok(expires_at)
}

/// Whether some grant of `ceiling` still carries every verb of `clause` on a
/// selector that contains one of the clause's selectors, whatever the
/// topology. A clause is the conjunction of its two selectors, so either
/// one staying covered keeps the clause inside the ceiling. A ceiling that
/// splits one clause across several grants is judged as not containing it:
/// the safe direction.
fn contains_clause(ceiling: &TerminalScopeSet, clause: &EffectiveClause) -> bool {
    ceiling.grants().iter().any(|grant| {
        verbs_cover(grant.verbs, clause.verbs)
            && (covers(&grant.selector, &clause.ceiling)
                || covers(&grant.selector, &clause.requested))
    })
}

const fn verbs_cover(have: Verbs, need: Verbs) -> bool {
    have.bits() & need.bits() == need.bits()
}

/// Whether `outer` contains every subject `inner` can contain, under any
/// topology. A Group contains a Terminal only while it is a member, so a
/// Group never statically covers a Terminal selector.
fn covers(outer: &Selector, inner: &Selector) -> bool {
    match (outer, inner) {
        (Selector::Global, _)
        | (Selector::HostLocal, Selector::Group(_) | Selector::TerminalLocal(_)) => true,
        (Selector::HostSatellite(host), Selector::TerminalSatellite(terminal_host, _)) => {
            host == terminal_host
        }
        _ => outer == inner,
    }
}

fn earliest(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn until(when: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (when - now).to_std().unwrap_or(Duration::ZERO)
}
