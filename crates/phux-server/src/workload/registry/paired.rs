//! Explicit repair of the legacy full-control pairing ceiling, the watch
//! grants pairing adds for a person's device, and the one-time migration
//! that gives them to devices paired before watching needed them. Normal
//! loads and HELLO never widen a credential, including a human credential.

use std::path::Path;

use chrono::Utc;

use super::{PairedDeviceRepair, WorkloadRegistry};
use crate::workload::{
    EnrolledFor, LEGACY_PAIRED_DEVICE_SCOPE, PAIRED_DEVICE_SCOPE, PAIRED_DEVICE_WATCH_SCOPES,
    WorkloadCredential, WorkloadError, is_canonical_credential_id, parse_ceiling, store,
};

impl WorkloadRegistry {
    /// Add the organization and termination permissions missing from an old
    /// paired device's full-control ceiling, under the registry lock.
    ///
    /// The owner must explicitly name a credential. Only an active human with
    /// exactly the legacy pairing ceiling qualifies; an already repaired
    /// device is a no-op. Keys and expiry are never changed. Existing live
    /// connections keep their minted grant until the device reconnects.
    ///
    /// # Errors
    ///
    /// An invalid or absent id, a non-paired/inactive credential, or any
    /// failure to securely read or commit the registry. Nothing is written
    /// for a refused repair.
    pub fn repair_paired_device(
        path: &Path,
        id: &str,
    ) -> Result<PairedDeviceRepair, WorkloadError> {
        if !is_canonical_credential_id(id) {
            return Err(WorkloadError::InvalidCredentialId);
        }
        store::with_lock(path, |held| {
            let mut registry = Self::load(path)?;
            let credential = registry
                .credentials
                .iter_mut()
                .find(|credential| credential.id == id)
                .ok_or_else(|| WorkloadError::NotFound(id.to_owned()))?;
            let updated = repair_ceiling(credential, Utc::now().timestamp())?;
            if updated {
                registry.commit(path, held)?;
            }
            Ok(PairedDeviceRepair {
                generation: registry.generation,
                updated,
            })
        })
    }
}

impl WorkloadRegistry {
    /// Once per registry file: give every active human device whose ceiling
    /// is exactly a full-control pairing ceiling (today's or the legacy one)
    /// the watch grants ([`PAIRED_DEVICE_WATCH_SCOPES`]), so a device paired
    /// before watching a window or display needed them keeps the view it
    /// had. A marker beside the file records that it ran, so ungranting
    /// them later is never undone. Returns the credentials it changed;
    /// `None` when it already ran or there is no registry yet.
    ///
    /// # Errors
    ///
    /// Any failure to read or commit the registry or write the marker.
    pub fn migrate_paired_watch(path: &Path) -> Result<Option<Vec<String>>, WorkloadError> {
        let marker = watch_marker(path);
        if marker.symlink_metadata().is_ok() || path.symlink_metadata().is_err() {
            return Ok(None);
        }
        store::with_lock(path, |held| {
            if marker.symlink_metadata().is_ok() {
                return Ok(None);
            }
            let mut registry = Self::load(path)?;
            let now = Utc::now().timestamp();
            let full = [
                parse_ceiling(&[PAIRED_DEVICE_SCOPE.to_owned()])?,
                parse_ceiling(&[LEGACY_PAIRED_DEVICE_SCOPE.to_owned()])?,
            ];
            let mut changed = Vec::new();
            for credential in &mut registry.credentials {
                let paired = credential.is_active_at(now)
                    && credential.satellite.is_none()
                    && credential.enrolled_for() == EnrolledFor::Human
                    && credential
                        .ceiling()
                        .is_ok_and(|ceiling| full.contains(&ceiling));
                if paired {
                    // The legacy principal is inferred from the ceiling
                    // about to change: pin it first.
                    credential.recorded_for = Some(EnrolledFor::Human);
                    credential.scopes = with_watch(std::mem::take(&mut credential.scopes));
                    changed.push(credential.id.clone());
                }
            }
            if !changed.is_empty() {
                registry.commit(path, held)?;
            }
            store::atomic_replace(
                &marker,
                b"paired devices were given observe@display and observe@window once\n",
                held,
            )?;
            Ok(Some(changed))
        })
    }
}

/// Where [`WorkloadRegistry::migrate_paired_watch`] records that it ran.
fn watch_marker(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workload-keys");
    store::parent_of(path).join(format!("{name}.watch-grants"))
}

/// `scopes` plus the watch grants a person's device paired with the
/// full-control ceiling holds.
fn with_watch(mut scopes: Vec<String>) -> Vec<String> {
    scopes.extend(PAIRED_DEVICE_WATCH_SCOPES.map(str::to_owned));
    scopes
}

/// The ceiling a new credential is enrolled with: a human device paired with
/// exactly the full-control ceiling also watches windows and displays
/// (`docs/authority.md`); every other request is kept as asked.
#[must_use]
pub fn enrolled_scopes(scopes: Vec<String>, enrolled_for: Option<EnrolledFor>) -> Vec<String> {
    let paired = enrolled_for == Some(EnrolledFor::Human)
        && parse_ceiling(&scopes).ok() == parse_ceiling(&[PAIRED_DEVICE_SCOPE.to_owned()]).ok();
    if paired { with_watch(scopes) } else { scopes }
}

fn repair_ceiling(credential: &mut WorkloadCredential, now: i64) -> Result<bool, WorkloadError> {
    if !credential.is_active_at(now)
        || credential.satellite.is_some()
        || credential.enrolled_for() != EnrolledFor::Human
    {
        return Err(not_paired());
    }
    let ceiling = credential.ceiling()?;
    let paired = vec![PAIRED_DEVICE_SCOPE.to_owned()];
    let legacy = vec![LEGACY_PAIRED_DEVICE_SCOPE.to_owned()];
    if ceiling == parse_ceiling(&paired)? || ceiling == parse_ceiling(&with_watch(paired.clone()))?
    {
        return Ok(false);
    }
    // A migrated legacy device keeps the watch grants it was given.
    credential.scopes = if ceiling == parse_ceiling(&legacy)? {
        paired
    } else if ceiling == parse_ceiling(&with_watch(legacy))? {
        with_watch(paired)
    } else {
        return Err(not_paired());
    };
    // Preserve an inferred legacy human after replacing the ceiling that
    // provided that inference. This changes no principal's authority by itself.
    credential.recorded_for = Some(EnrolledFor::Human);
    Ok(true)
}

fn not_paired() -> WorkloadError {
    WorkloadError::Malformed(
        "repair requires an active human device with exactly the legacy full-control pairing ceiling; no permissions changed".to_owned(),
    )
}
