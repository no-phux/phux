//! Validation of scopes, expiry, and satellite names; credential ids; and
//! the default material paths.

use std::path::PathBuf;

use chrono::Utc;
use phux_protocol::scope::{EffectiveScopeSet, ScopeGrant, TerminalScopeSet};
use sha2::{Digest, Sha256};

use super::{DIGEST_PREFIX, MAX_EXPIRY_SECONDS, WorkloadError};

/// A satellite name obeys the federation host-key rules (`docs/remote.md`).
///
/// It is 1..=255 UTF-8 bytes, no NUL or control character. A `/` or `:` is
/// refused too, as `phux host` refuses them in a registry name.
///
/// # Errors
///
/// [`WorkloadError::Malformed`].
pub fn validate_satellite_name(name: &str) -> Result<(), WorkloadError> {
    let valid = !name.is_empty()
        && name.len() <= 255
        && !name.chars().any(|c| c.is_control() || c == '/' || c == ':');
    if valid {
        Ok(())
    } else {
        Err(WorkloadError::Malformed(
            "a satellite name is 1 to 255 bytes with no control character, '/', or ':'".to_owned(),
        ))
    }
}

/// Every scope must parse, there must be at least one, and together they
/// must fit one canonical set. One bad string refuses the whole record:
/// there is no partial grant.
///
/// # Errors
///
/// [`WorkloadError::NoScopes`], the first [`WorkloadError::InvalidScope`],
/// or [`WorkloadError::Malformed`] for more than 64 distinct selectors.
pub fn validate_scopes(scopes: &[String]) -> Result<(), WorkloadError> {
    parse_ceiling(scopes).map(drop)
}

pub(super) fn parse_ceiling(scopes: &[String]) -> Result<TerminalScopeSet, WorkloadError> {
    if scopes.is_empty() {
        return Err(WorkloadError::NoScopes);
    }
    let ceiling = TerminalScopeSet::parse_all(scopes).map_err(|(index, rule)| {
        rule.map_or_else(
            || {
                WorkloadError::Malformed(
                    "a scope ceiling names more than 64 distinct selectors".to_owned(),
                )
            },
            |reason| WorkloadError::InvalidScope {
                index: index + 1,
                reason,
            },
        )
    })?;
    refuse_restartable_ids(scopes)?;
    // Admission mints exactly this effective set (workload-auth §5.1), so a
    // ceiling it cannot hold is refused here, at enrollment and load, rather
    // than at every HELLO.
    EffectiveScopeSet::unattenuated(&ceiling).map_err(|_| {
        WorkloadError::Malformed("a scope ceiling encodes larger than section 5 allows".to_owned())
    })?;
    Ok(ceiling)
}

/// Refuse `group:` and `terminal:` selectors in a persisted ceiling: the ids
/// they name restart with the server, so a registry record would come to name
/// a different session or Terminal (workload-auth §5.1). The keyed
/// `session:` and `resource:` selectors name records that outlive it.
fn refuse_restartable_ids(scopes: &[String]) -> Result<(), WorkloadError> {
    let restartable = |scope: &String| {
        ScopeGrant::parse(scope).is_ok_and(|grant| grant.selector.names_process_id())
    };
    scopes.iter().position(restartable).map_or(Ok(()), |index| {
        Err(WorkloadError::UnstableSelector { index: index + 1 })
    })
}

/// An expiry must lie after now and within [`MAX_EXPIRY_SECONDS`].
///
/// # Errors
///
/// [`WorkloadError::InvalidExpiry`].
pub fn validate_expiry(expires_at: i64) -> Result<(), WorkloadError> {
    let now = Utc::now().timestamp();
    if expires_at > now && expires_at - now <= MAX_EXPIRY_SECONDS {
        Ok(())
    } else {
        Err(WorkloadError::InvalidExpiry)
    }
}

/// Canonical workload credential id for a raw public key.
#[must_use]
pub fn credential_id(public_key: &[u8]) -> String {
    format!("{DIGEST_PREFIX}{}", hex::encode(Sha256::digest(public_key)))
}

/// Whether `id` is `sha256:` followed by exactly 64 lowercase hex digits.
#[must_use]
pub fn is_canonical_credential_id(id: &str) -> bool {
    id.strip_prefix(DIGEST_PREFIX).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Overwrite a buffer that held key material before it is dropped. Best
/// effort: an allocator or a crypto library may already hold another copy.
pub fn scrub(buffer: &mut [u8]) {
    buffer.fill(0);
    std::hint::black_box(buffer);
}

/// Default persisted CA certificate path.
#[must_use]
pub fn default_ca_cert_path() -> PathBuf {
    crate::telemetry::state_dir().join("workload-ca.pem")
}

/// Default persisted CA private-key path.
#[must_use]
pub fn default_ca_key_path() -> PathBuf {
    crate::telemetry::state_dir().join("workload-ca.key")
}

/// Default persisted workload-key registry path.
#[must_use]
pub fn default_registry_path() -> PathBuf {
    crate::telemetry::state_dir().join("workload-keys")
}
