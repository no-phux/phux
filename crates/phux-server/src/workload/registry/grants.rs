//! Adding or removing one scope grant on an enrolled credential, without
//! re-pairing (`phux workload grant|ungrant`).

use std::path::Path;

use chrono::Utc;
use phux_protocol::scope::ScopeGrant;

use super::WorkloadRegistry;
use crate::workload::{WorkloadError, is_canonical_credential_id, parse_ceiling, store};

/// Result of a scope grant or ungrant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeChange {
    /// Registry generation after the operation.
    pub generation: u64,
    /// Whether the ceiling changed; false when it already held (or lacked)
    /// the grant.
    pub updated: bool,
    /// The credential's scopes after the operation.
    pub scopes: Vec<String>,
}

impl WorkloadRegistry {
    /// Add `scope` to the active credential `id`'s ceiling (`add`), or remove
    /// every scope equal to it, under the registry lock, keeping its key,
    /// expiry and principal. A running server mints the new ceiling at the
    /// device's next connection; a removal also ends live connections the
    /// old ceiling admitted, as the revocation watcher judges.
    ///
    /// # Errors
    ///
    /// An invalid or absent id; a revoked, expired, or satellite credential;
    /// a scope that does not parse or that the ceiling cannot hold; removing
    /// the last scope (revoke instead); or a failure to read or commit the
    /// registry. Nothing is written for a refused change.
    pub fn change_scope(
        path: &Path,
        id: &str,
        scope: &str,
        add: bool,
    ) -> Result<ScopeChange, WorkloadError> {
        if !is_canonical_credential_id(id) {
            return Err(WorkloadError::InvalidCredentialId);
        }
        let grant = ScopeGrant::parse(scope)
            .map_err(|reason| WorkloadError::InvalidScope { index: 1, reason })?;
        store::with_lock(path, |held| {
            let mut registry = Self::load(path)?;
            let credential = registry
                .credentials
                .iter_mut()
                .find(|credential| credential.id == id)
                .ok_or_else(|| WorkloadError::NotFound(id.to_owned()))?;
            if !credential.is_active_at(Utc::now().timestamp()) || credential.satellite.is_some() {
                return Err(WorkloadError::Malformed(
                    "only an active device or workload credential takes a scope; a satellite's holds none".to_owned(),
                ));
            }
            let same = |text: &String| ScopeGrant::parse(text).is_ok_and(|have| have == grant);
            let mut scopes = credential.scopes.clone();
            if add {
                if !scopes.iter().any(same) {
                    scopes.push(grant.to_string());
                }
            } else {
                scopes.retain(|text| !same(text));
                if scopes.is_empty() {
                    return Err(WorkloadError::Malformed(
                        "a credential keeps at least one scope; revoke it instead".to_owned(),
                    ));
                }
            }
            let updated = scopes != credential.scopes;
            if updated {
                parse_ceiling(&scopes)?;
                // A legacy record infers its principal from its ceiling: pin it
                // before the ceiling changes.
                credential.recorded_for = Some(credential.enrolled_for());
                credential.scopes.clone_from(&scopes);
                registry.commit(path, held)?;
            }
            Ok(ScopeChange {
                generation: registry.generation,
                updated,
                scopes,
            })
        })
    }
}
