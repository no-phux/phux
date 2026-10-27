//! Explicit repair of the legacy full-control pairing ceiling. Normal loads
//! and HELLO never widen a credential, including a human credential.

use std::path::Path;

use chrono::Utc;

use super::{PairedDeviceRepair, WorkloadRegistry};
use crate::workload::{
    EnrolledFor, LEGACY_PAIRED_DEVICE_SCOPE, PAIRED_DEVICE_SCOPE, WorkloadCredential,
    WorkloadError, is_canonical_credential_id, parse_ceiling, store,
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

fn repair_ceiling(credential: &mut WorkloadCredential, now: i64) -> Result<bool, WorkloadError> {
    if !credential.is_active_at(now)
        || credential.satellite.is_some()
        || credential.enrolled_for() != EnrolledFor::Human
    {
        return Err(not_paired());
    }
    let ceiling = credential.ceiling()?;
    if ceiling == parse_ceiling(&[PAIRED_DEVICE_SCOPE.to_owned()])? {
        return Ok(false);
    }
    if ceiling != parse_ceiling(&[LEGACY_PAIRED_DEVICE_SCOPE.to_owned()])? {
        return Err(not_paired());
    }
    credential.scopes = vec![PAIRED_DEVICE_SCOPE.to_owned()];
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
