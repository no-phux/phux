//! Advisory path leases. The address identifies the coordination principal;
//! the fence identifies one acquisition, including all its paths. Neither a
//! label nor a process-liveness observation authorizes another incarnation.

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::{Address, append_event};
use crate::store::timestamp;
use crate::{Error, Store};

const MAX_CLAIM_PATHS: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
const MAX_NOTE_BYTES: usize = 512;
/// Longest lease lifetime. Expiry, not process death, frees forgotten claims.
pub const MAX_CLAIM_SECS: u64 = 24 * 60 * 60;

/// Acquire a new lease. This never implicitly renews an existing acquisition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeClaim {
    /// Coordination address, not a secret or an authentication credential.
    pub holder: String,
    /// Absolute paths, acquired atomically under one fence.
    pub paths: Vec<String>,
    /// Exclude every other acquisition, including this address's other leases.
    pub exclusive: bool,
    /// Lifetime, from 1 second through [`MAX_CLAIM_SECS`].
    pub ttl_secs: u64,
    /// Reason shown to other agents.
    pub note: String,
}

/// One path in a live lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The path and everything beneath it.
    pub path: String,
    /// Address as written; labels are display, not different owners.
    pub holder: String,
    /// Whether overlapping acquisitions are excluded.
    pub exclusive: bool,
    /// The incarnation shared by every path acquired together.
    pub fence: u64,
    /// Why the path is held.
    pub note: String,
    /// Authoritative expiry, in Unix seconds.
    pub expires_at: u64,
}

/// Acquisition either grants every path or changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "claims", rename_all = "snake_case")]
pub enum ClaimOutcome {
    /// All paths, sharing one new fence.
    Granted(Vec<Claim>),
    /// The live acquisitions that prevented the request.
    Conflict(Vec<Claim>),
}

/// Extend exactly one live acquisition without changing its paths or mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenewClaim {
    /// Address that acquired the lease.
    pub holder: String,
    /// Fence returned by that acquisition, not a newly discovered replacement.
    pub fence: u64,
    /// Lifetime from the current time, within [`MAX_CLAIM_SECS`].
    pub ttl_secs: u64,
}

/// Release exactly one acquisition. Repeating its cleanup is harmless.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseClaim {
    /// Address that acquired the lease.
    pub holder: String,
    /// Fence returned by that acquisition. No name-only or release-all mutation.
    pub fence: u64,
}

/// Read-only selection of live claims.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFilter {
    /// Include paths overlapping this absolute path.
    pub path: Option<String>,
    /// Include this canonical address, regardless of display label.
    pub holder: Option<String>,
}

impl Store {
    /// Acquire every requested path under one new fence, or none on conflict.
    ///
    /// # Errors
    /// Validation and store errors. Conflicts are ordinary outcomes.
    pub fn take_claim(&mut self, request: &TakeClaim) -> Result<ClaimOutcome, Error> {
        let (holder, paths) = checked_acquisition(request)?;
        let (tx, now) = live_transaction(&mut self.conn)?;
        let conflicts = conflicts(&load_claims(&tx)?, &paths, holder.key(), request.exclusive);
        if !conflicts.is_empty() {
            return Ok(ClaimOutcome::Conflict(conflicts));
        }
        let fence = next_fence(&tx)?;
        let expires_at = now.saturating_add(request.ttl_secs);
        let mut granted = Vec::with_capacity(paths.len());
        for path in paths {
            tx.execute(
                "INSERT INTO claim (path, holder, holder_key, exclusive, fence, note, expires_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![&path, holder.display(), holder.key(), request.exclusive, fence, &request.note, sql_secs(expires_at), timestamp()],
            )?;
            granted.push(Claim {
                path,
                holder: holder.display().to_owned(),
                exclusive: request.exclusive,
                fence: u64::try_from(fence).unwrap_or(0),
                note: request.note.clone(),
                expires_at,
            });
        }
        append_event(&tx, holder.display(), "claim_taken", &granted)?;
        tx.commit()?;
        Ok(ClaimOutcome::Granted(granted))
    }

    /// Renew only a still-live acquisition belonging to this holder and fence.
    ///
    /// # Errors
    /// [`Error::NoClaim`] for expired, foreign or superseded receipts; validation
    /// and store errors. A failed renewal never creates an acquisition.
    pub fn renew_claim(&mut self, request: &RenewClaim) -> Result<Vec<Claim>, Error> {
        let holder = Address::parse(&request.holder)?;
        check_ttl(request.ttl_secs)?;
        let (tx, now) = live_transaction(&mut self.conn)?;
        let renewed = renew_rows(&tx, &holder, request, now.saturating_add(request.ttl_secs))?;
        append_event(&tx, holder.display(), "claim_renewed", &renewed)?;
        tx.commit()?;
        Ok(renewed)
    }

    /// Release precisely this holder's acquisition. Stale cleanup returns zero
    /// and cannot delete a replacement, even at the same address and path.
    ///
    /// # Errors
    /// Invalid addresses/fences and store errors.
    pub fn release_claim(&mut self, request: &ReleaseClaim) -> Result<u64, Error> {
        let holder = Address::parse(&request.holder)?;
        let fence = checked_fence(request.fence)?;
        let (tx, _) = live_transaction(&mut self.conn)?;
        let released = tx.execute(
            "DELETE FROM claim WHERE holder_key = ?1 AND fence = ?2",
            params![holder.key(), fence],
        )?;
        if released > 0 {
            append_event(
                &tx,
                holder.display(),
                "claim_released",
                &serde_json::json!({"fence": request.fence, "released": released}),
            )?;
        }
        tx.commit()?;
        Ok(u64::try_from(released).unwrap_or(u64::MAX))
    }

    /// Live claims only. Reading does not infer anything from a process state.
    ///
    /// # Errors
    /// Invalid filters and store errors.
    pub fn claims(&mut self, filter: &ClaimFilter) -> Result<Vec<Claim>, Error> {
        let path = filter.path.as_deref().map(parse_path).transpose()?;
        let holder = filter.holder.as_deref().map(Address::parse).transpose()?;
        let (tx, _) = live_transaction(&mut self.conn)?;
        let claims = load_claims(&tx)?
            .into_iter()
            .filter(|row| {
                path.as_deref()
                    .is_none_or(|path| overlaps(path, &row.claim.path))
                    && holder.as_ref().is_none_or(|holder| holder.key() == row.key)
            })
            .map(|row| row.claim)
            .collect();
        tx.commit()?;
        Ok(claims)
    }
}

/// Capture the clock only after obtaining the write transaction, then remove
/// expired rows in the same snapshot every lifecycle operation will use.
fn live_transaction(conn: &mut Connection) -> Result<(Transaction<'_>, u64), Error> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = now_secs();
    sweep(&tx, now)?;
    Ok((tx, now))
}

fn checked_acquisition(request: &TakeClaim) -> Result<(Address, Vec<String>), Error> {
    let holder = Address::parse(&request.holder)?;
    let paths = checked_paths(&request.paths)?;
    check_ttl(request.ttl_secs)?;
    check_note(&request.note)?;
    Ok((holder, paths))
}

fn renew_rows(
    tx: &Transaction<'_>,
    holder: &Address,
    request: &RenewClaim,
    expires_at: u64,
) -> Result<Vec<Claim>, Error> {
    let fence = checked_fence(request.fence)?;
    let mut renewed: Vec<Claim> = load_claims(tx)?
        .into_iter()
        .filter(|row| row.key == holder.key() && row.claim.fence == request.fence)
        .map(|row| row.claim)
        .collect();
    if renewed.is_empty() {
        return Err(Error::NoClaim {
            fence: request.fence,
        });
    }
    tx.execute(
        "UPDATE claim SET expires_at = ?1 WHERE holder_key = ?2 AND fence = ?3",
        params![sql_secs(expires_at), holder.key(), fence],
    )?;
    for claim in &mut renewed {
        claim.expires_at = expires_at;
    }
    Ok(renewed)
}

fn checked_paths(paths: &[String]) -> Result<Vec<String>, Error> {
    if paths.is_empty() || paths.len() > MAX_CLAIM_PATHS {
        return Err(Error::invalid_text("paths", "name 1 to 64 paths"));
    }
    let mut checked = Vec::with_capacity(paths.len());
    for path in paths {
        let path = parse_path(path)?;
        if !checked.contains(&path) {
            checked.push(path);
        }
    }
    Ok(checked)
}

fn check_ttl(ttl: u64) -> Result<(), Error> {
    if ttl == 0 || ttl > MAX_CLAIM_SECS {
        return Err(Error::invalid_text("ttl", "must be 1 second to 24 hours"));
    }
    Ok(())
}

fn check_note(note: &str) -> Result<(), Error> {
    if note.len() > MAX_NOTE_BYTES || note.contains('\0') {
        return Err(Error::invalid_text(
            "note",
            "is over 512 bytes or contains NUL",
        ));
    }
    Ok(())
}

fn checked_fence(fence: u64) -> Result<i64, Error> {
    i64::try_from(fence)
        .ok()
        .filter(|fence| *fence > 0)
        .ok_or_else(|| Error::invalid_text("fence", "must be a positive signed 64-bit integer"))
}

fn parse_path(text: &str) -> Result<String, Error> {
    let invalid = |reason| Error::invalid_text("path", reason);
    if !text.starts_with('/') || text.len() > MAX_PATH_BYTES || text.contains('\0') {
        return Err(invalid("must be an absolute path under 4 KiB"));
    }
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(invalid("cannot claim the whole filesystem"));
    }
    if trimmed
        .split('/')
        .skip(1)
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid(
            "must be normalized, without empty, `.` or `..` segments",
        ));
    }
    Ok(trimmed.to_owned())
}

fn overlaps(a: &str, b: &str) -> bool {
    fn within(inner: &str, outer: &str) -> bool {
        inner == outer
            || inner
                .strip_prefix(outer)
                .is_some_and(|rest| rest.starts_with('/'))
    }
    within(a, b) || within(b, a)
}

struct HeldClaim {
    key: String,
    claim: Claim,
}

fn load_claims(tx: &Transaction<'_>) -> Result<Vec<HeldClaim>, Error> {
    let mut statement = tx.prepare("SELECT path, holder, holder_key, exclusive, fence, note, expires_at FROM claim ORDER BY path, fence")?;
    let rows = statement.query_map([], |row| {
        let fence: i64 = row.get(4)?;
        let expires: i64 = row.get(6)?;
        Ok(HeldClaim {
            key: row.get(2)?,
            claim: Claim {
                path: row.get(0)?,
                holder: row.get(1)?,
                exclusive: row.get(3)?,
                fence: u64::try_from(fence).unwrap_or(0),
                note: row.get(5)?,
                expires_at: u64::try_from(expires).unwrap_or(0),
            },
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
}

fn conflicts(live: &[HeldClaim], paths: &[String], holder: &str, exclusive: bool) -> Vec<Claim> {
    live.iter()
        .filter(|row| {
            let same_key = row.key == holder && paths.contains(&row.claim.path);
            let exclusive_overlap = (exclusive || row.claim.exclusive)
                && paths.iter().any(|path| overlaps(path, &row.claim.path));
            same_key || exclusive_overlap
        })
        .map(|row| row.claim.clone())
        .collect()
}

fn sweep(tx: &Transaction<'_>, now: u64) -> Result<(), Error> {
    tx.execute(
        "DELETE FROM claim WHERE expires_at <= ?1",
        params![sql_secs(now)],
    )?;
    Ok(())
}

fn next_fence(tx: &Transaction<'_>) -> Result<i64, Error> {
    Ok(tx.query_row(
        "INSERT INTO claim_fence (id, next) VALUES (1, 1)
         ON CONFLICT(id) DO UPDATE SET next = next + 1 RETURNING next",
        [],
        |row| row.get(0),
    )?)
}

fn now_secs() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0)
}
fn sql_secs(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("work/work-v1.sqlite")).unwrap();
        (dir, store)
    }
    fn take(store: &mut Store, holder: &str, paths: &[&str], exclusive: bool) -> ClaimOutcome {
        store
            .take_claim(&TakeClaim {
                holder: holder.into(),
                paths: paths.iter().map(|p| (*p).into()).collect(),
                exclusive,
                ttl_secs: 600,
                note: "test".into(),
            })
            .unwrap()
    }
    fn granted(store: &mut Store, holder: &str, paths: &[&str]) -> Vec<Claim> {
        let ClaimOutcome::Granted(claims) = take(store, holder, paths, true) else {
            panic!("free path")
        };
        claims
    }
    fn expire(store: &Store) {
        // Disposable regression fixture only, never live lease administration.
        store
            .conn
            .execute("UPDATE claim SET expires_at = 1", [])
            .unwrap();
    }
    #[test]
    fn same_pane_children_cannot_implicitly_adopt_another_acquisition() {
        let (_dir, mut store) = store();
        let first = granted(&mut store, "pi@749", &["/r"]);
        assert!(matches!(
            take(&mut store, "child@749", &["/r"], true),
            ClaimOutcome::Conflict(_)
        ));
        assert!(matches!(
            take(&mut store, "child@749", &["/r/src"], true),
            ClaimOutcome::Conflict(_)
        ));
        assert_eq!(store.claims(&ClaimFilter::default()).unwrap(), first);
    }
    #[test]
    fn a_stale_receipt_cannot_renew_or_release_a_replacement() {
        let (_dir, mut store) = store();
        let first = granted(&mut store, "@749", &["/r"]);
        let cleanup = ReleaseClaim {
            holder: "@749".into(),
            fence: first[0].fence,
        };
        expire(&store);
        let next = granted(&mut store, "@749", &["/r"]);
        assert!(next[0].fence > first[0].fence);
        assert!(matches!(
            store.renew_claim(&RenewClaim {
                holder: "@749".into(),
                fence: first[0].fence,
                ttl_secs: 900
            }),
            Err(Error::NoClaim { .. })
        ));
        assert_eq!(store.release_claim(&cleanup).unwrap(), 0);
        assert_eq!(store.claims(&ClaimFilter::default()).unwrap(), next);
    }
    #[test]
    fn an_expired_renewal_never_creates_a_new_acquisition() {
        let (_dir, mut store) = store();
        let first = granted(&mut store, "a", &["/r"]);
        expire(&store);
        assert!(matches!(
            store.renew_claim(&RenewClaim {
                holder: "a".into(),
                fence: first[0].fence,
                ttl_secs: 900
            }),
            Err(Error::NoClaim { .. })
        ));
        assert!(store.claims(&ClaimFilter::default()).unwrap().is_empty());
    }
    #[test]
    fn one_atomic_acquisition_has_one_fence_and_renews_as_a_whole() {
        let (_dir, mut store) = store();
        let first = granted(&mut store, "pi@749", &["/r/a", "/r/z", "/r/a"]);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].fence, first[1].fence);
        let renewed = store
            .renew_claim(&RenewClaim {
                holder: "@749".into(),
                fence: first[0].fence,
                ttl_secs: 1200,
            })
            .unwrap();
        assert_eq!(renewed.len(), 2);
        for (before, after) in first.iter().zip(&renewed) {
            assert_eq!(before.fence, after.fence);
            assert_eq!(before.path, after.path);
            assert_eq!(before.note, after.note);
            assert_eq!(before.exclusive, after.exclusive);
            assert!(after.expires_at > before.expires_at);
        }
    }
    #[test]
    fn foreign_receipts_and_repeated_cleanup_cannot_remove_live_work() {
        let (_dir, mut store) = store();
        let first = granted(&mut store, "@749", &["/r/a", "/r/b"]);
        let fence = first[0].fence;
        assert!(matches!(
            store.renew_claim(&RenewClaim {
                holder: "@730".into(),
                fence,
                ttl_secs: 100
            }),
            Err(Error::NoClaim { .. })
        ));
        assert_eq!(
            store
                .release_claim(&ReleaseClaim {
                    holder: "@730".into(),
                    fence
                })
                .unwrap(),
            0
        );
        assert_eq!(store.claims(&ClaimFilter::default()).unwrap(), first);
        let cleanup = ReleaseClaim {
            holder: "pi@749".into(),
            fence,
        };
        assert_eq!(store.release_claim(&cleanup).unwrap(), 2);
        assert_eq!(store.release_claim(&cleanup).unwrap(), 0);
    }
    #[test]
    fn handing_off_releases_then_acquires_a_new_fence() {
        let (_dir, mut store) = store();
        let first = granted(&mut store, "a", &["/r"]);
        assert!(matches!(
            take(&mut store, "b", &["/r/src"], true),
            ClaimOutcome::Conflict(_)
        ));
        store
            .release_claim(&ReleaseClaim {
                holder: "a".into(),
                fence: first[0].fence,
            })
            .unwrap();
        let next = granted(&mut store, "b", &["/r/src"]);
        assert!(next[0].fence > first[0].fence);
    }
    #[test]
    fn shared_leases_coexist_and_conflicting_batches_change_nothing() {
        let (_dir, mut store) = store();
        assert!(matches!(
            take(&mut store, "a", &["/r"], false),
            ClaimOutcome::Granted(_)
        ));
        assert!(matches!(
            take(&mut store, "b", &["/r/x"], false),
            ClaimOutcome::Granted(_)
        ));
        assert!(matches!(
            take(&mut store, "a", &["/r"], false),
            ClaimOutcome::Conflict(_)
        ));
        assert!(matches!(
            take(&mut store, "c", &["/elsewhere", "/r/y"], true),
            ClaimOutcome::Conflict(_)
        ));
        assert!(
            store
                .claims(&ClaimFilter {
                    path: Some("/elsewhere".into()),
                    holder: None
                })
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            take(&mut store, "c", &["/r-other"], true),
            ClaimOutcome::Granted(_)
        ));
    }
    #[test]
    fn expiry_allows_a_fresh_holder_and_fences_survive_reopening() {
        let (dir, mut store) = store();
        let first = granted(&mut store, "a", &["/r"]);
        expire(&store);
        drop(store);
        let mut store = Store::open(&dir.path().join("work/work-v1.sqlite")).unwrap();
        let next = granted(&mut store, "b", &["/r"]);
        assert!(next[0].fence > first[0].fence);
    }
    #[test]
    fn root_aliases_and_non_normalized_paths_are_rejected() {
        for bad in ["relative", "/", "//", "///", "/a/../b", "/a//b", "/a/./b"] {
            assert!(parse_path(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_path("/a/b/").unwrap(), "/a/b");
        assert!(overlaps("/a", "/a/b") && overlaps("/a/b", "/a") && !overlaps("/a", "/ab"));
    }
    #[test]
    fn invalid_lifetimes_and_fences_do_not_mutate_a_lease() {
        let (_dir, mut store) = store();
        let first = granted(&mut store, "a", &["/r"]);
        for ttl_secs in [0, MAX_CLAIM_SECS + 1] {
            assert!(
                store
                    .renew_claim(&RenewClaim {
                        holder: "a".into(),
                        fence: first[0].fence,
                        ttl_secs
                    })
                    .is_err()
            );
        }
        for fence in [0, u64::MAX] {
            assert!(
                store
                    .release_claim(&ReleaseClaim {
                        holder: "a".into(),
                        fence
                    })
                    .is_err()
            );
        }
        assert_eq!(store.claims(&ClaimFilter::default()).unwrap(), first);
    }
}
