//! Single-use enrollment tickets (`workload-auth.md` §8.2, ADR-0154).
//!
//! `phux pair --enroll` mints one: a 256-bit secret the connect link carries,
//! of which this store keeps only the SHA-256, beside the scope ceiling and
//! lifetime the credential it enrolls will get and the ticket's own expiry.
//! The first redemption consumes it, under the store lock, before anything
//! is issued, so two devices racing one ticket enroll at most one key. The
//! file sits beside the registry and obeys its rules (owner-only, no-follow,
//! locked atomic replacement).

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::store::{self, FileRole};
use super::{WorkloadError, validate_scopes};

/// File name beside the workload registry.
pub const TICKETS_FILE: &str = "enrollment-tickets";

/// Bytes of ticket secret.
pub const TICKET_BYTES: usize = 32;

/// Default ticket lifetime: long enough to scan a QR code.
pub const DEFAULT_TICKET_SECONDS: i64 = 10 * 60;

/// Default lifetime of the credential a ticket enrolls: a device without ssh
/// cannot renew unattended, so a year rather than `add-key`'s 90 days.
pub const DEFAULT_CREDENTIAL_SECONDS: i64 = 365 * 24 * 60 * 60;

/// Consumed or expired tickets are kept this long (for diagnosis), then
/// pruned at the next mint.
const KEEP_SPENT_SECONDS: i64 = 24 * 60 * 60;

const STORE_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketFile {
    version: u32,
    tickets: Vec<TicketRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketRecord {
    /// Non-secret handle, logged at mint and redemption.
    id: String,
    /// Lowercase hex SHA-256 of the secret.
    secret_sha256: String,
    scopes: Vec<String>,
    credential_seconds: i64,
    expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_at: Option<i64>,
}

/// A freshly minted ticket. The secret is shown once and never stored.
pub struct MintedTicket {
    /// Non-secret handle.
    pub id: String,
    /// The secret, lowercase hex: what the link carries.
    pub secret_hex: String,
    /// When it stops being redeemable (Unix seconds).
    pub expires_at: i64,
}

impl std::fmt::Debug for MintedTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedTicket")
            .field("id", &self.id)
            .field("secret_hex", &"[withheld]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// What a redeemed ticket authorizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redeemed {
    /// The ticket's handle.
    pub id: String,
    /// The scope ceiling to enroll with.
    pub scopes: Vec<String>,
    /// The credential's absolute expiry (Unix seconds).
    pub credential_expires_at: i64,
}

/// The tickets store beside the registry at `registry`.
#[must_use]
pub fn tickets_path(registry: &Path) -> PathBuf {
    registry.with_file_name(TICKETS_FILE)
}

/// Mint a ticket into the store at `path`.
///
/// # Errors
///
/// An invalid scope, a non-positive lifetime, or a store failure.
pub fn mint_ticket(
    path: &Path,
    scopes: Vec<String>,
    credential_seconds: i64,
    ticket_seconds: i64,
) -> Result<MintedTicket, WorkloadError> {
    validate_scopes(&scopes)?;
    if credential_seconds <= 0
        || credential_seconds > super::MAX_EXPIRY_SECONDS
        || ticket_seconds <= 0
    {
        return Err(WorkloadError::InvalidExpiry);
    }
    let mut secret = [0_u8; TICKET_BYTES];
    getrandom::fill(&mut secret).map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut handle = [0_u8; 4];
    getrandom::fill(&mut handle).map_err(|error| std::io::Error::other(error.to_string()))?;
    let now = Utc::now().timestamp();
    let record = TicketRecord {
        id: format!("t-{}", hex::encode(handle)),
        secret_sha256: hex::encode(Sha256::digest(secret)),
        scopes,
        credential_seconds,
        expires_at: now.saturating_add(ticket_seconds),
        used_at: None,
    };
    let minted = MintedTicket {
        id: record.id.clone(),
        secret_hex: hex::encode(secret),
        expires_at: record.expires_at,
    };
    super::scrub(&mut secret);
    store::with_lock(path, |held| {
        let mut file = load(path)?;
        file.tickets.retain(|ticket| {
            let spent = ticket.used_at.unwrap_or(ticket.expires_at);
            spent.saturating_add(KEEP_SPENT_SECONDS) > now
        });
        file.tickets.push(record);
        commit(path, &file, held)
    })?;
    Ok(minted)
}

/// Consume the ticket whose secret is `secret` (raw bytes), if one is live.
///
/// Every record is compared in constant time, and a redemption that matches
/// nothing, an expired ticket, and a consumed one all return
/// [`WorkloadError::NotFound`] with no detail.
///
/// # Errors
///
/// [`WorkloadError::NotFound`] when no live ticket matches, or a store
/// failure.
pub fn redeem_ticket(path: &Path, secret: &[u8]) -> Result<Redeemed, WorkloadError> {
    let digest = hex::encode(Sha256::digest(secret));
    let now = Utc::now().timestamp();
    store::with_lock(path, |held| {
        let mut file = load(path)?;
        let mut found = None;
        for (index, ticket) in file.tickets.iter().enumerate() {
            let matches: bool = ticket
                .secret_sha256
                .as_bytes()
                .ct_eq(digest.as_bytes())
                .into();
            if matches {
                found = Some(index);
            }
        }
        let refused = || WorkloadError::NotFound("enrollment ticket".to_owned());
        let index = found.ok_or_else(refused)?;
        let ticket = &mut file.tickets[index];
        if ticket.used_at.is_some() || ticket.expires_at <= now {
            return Err(refused());
        }
        ticket.used_at = Some(now);
        let redeemed = Redeemed {
            id: ticket.id.clone(),
            scopes: ticket.scopes.clone(),
            credential_expires_at: now.saturating_add(ticket.credential_seconds),
        };
        commit(path, &file, held)?;
        Ok(redeemed)
    })
}

/// A device enrolled with a ticket.
#[derive(Debug, Clone)]
pub struct TicketEnrollment {
    /// The ticket it redeemed.
    pub ticket: String,
    /// The credential id the registry now holds.
    pub credential: String,
    /// The issued chain, PEM: the client certificate, then the CA.
    pub chain_pem: String,
}

/// Enroll the key `csr_der` certifies, authorized by the ticket `secret`.
///
/// `workload-auth.md` §8.2: parse the request (no side effect), consume
/// the ticket, create the CA if absent, issue, and record the credential with
/// the ticket's ceiling and lifetime.
///
/// # Errors
///
/// A malformed request (the ticket is then left unspent), a ticket that is
/// not live, or any CA, signing, or registry failure after it was consumed.
pub fn enroll_with_ticket(
    paths: &super::WorkloadPaths,
    secret: &[u8],
    csr_der: &[u8],
) -> Result<TicketEnrollment, WorkloadError> {
    let material = super::ClientMaterial::from_request_der(csr_der)?;
    let redeemed = redeem_ticket(&tickets_path(&paths.registry), secret)?;
    super::ensure_ca(&paths.ca_cert, &paths.ca_key)?;
    let prepared = super::prepare_enrollment(paths, &material, redeemed.credential_expires_at)?;
    let chain_pem = prepared
        .issued_chain_pem()
        .ok_or(WorkloadError::MalformedAuthority("a CSR issued no chain"))?
        .to_owned();
    let registered = prepared.commit(
        &paths.registry,
        redeemed.scopes,
        redeemed.credential_expires_at,
    )?;
    Ok(TicketEnrollment {
        ticket: redeemed.id,
        credential: registered.id,
        chain_pem,
    })
}

fn load(path: &Path) -> Result<TicketFile, WorkloadError> {
    let Some((_, raw)) = store::read_stable(path, FileRole::Tickets)? else {
        return Ok(TicketFile {
            version: STORE_VERSION,
            tickets: Vec::new(),
        });
    };
    let file: TicketFile = serde_json::from_slice(&raw)
        .map_err(|error| WorkloadError::Malformed(format!("enrollment tickets: {error}")))?;
    if file.version != STORE_VERSION {
        return Err(WorkloadError::Malformed(
            "enrollment tickets: unknown version".to_owned(),
        ));
    }
    Ok(file)
}

fn commit(path: &Path, file: &TicketFile, held: &store::HeldLock<'_>) -> Result<(), WorkloadError> {
    let mut bytes = serde_json::to_vec_pretty(file)
        .map_err(|error| WorkloadError::Malformed(error.to_string()))?;
    bytes.push(b'\n');
    store::atomic_replace(path, &bytes, held)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let path = dir.path().join(TICKETS_FILE);
        (dir, path)
    }

    fn scopes() -> Vec<String> {
        vec!["observe,input@host".to_owned()]
    }

    #[test]
    fn a_ticket_redeems_once_and_the_store_holds_only_its_hash() {
        let (_dir, path) = store();
        let minted = mint_ticket(&path, scopes(), 3600, 600).unwrap();
        let secret = hex::decode(&minted.secret_hex).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains(&minted.secret_hex),
            "the secret is never stored"
        );
        assert!(!format!("{minted:?}").contains(&minted.secret_hex));

        let redeemed = redeem_ticket(&path, &secret).unwrap();
        assert_eq!(redeemed.id, minted.id);
        assert_eq!(redeemed.scopes, scopes());
        assert!(redeemed.credential_expires_at > Utc::now().timestamp() + 3000);

        // Adversarial: replay of a consumed ticket.
        assert!(matches!(
            redeem_ticket(&path, &secret),
            Err(WorkloadError::NotFound(_))
        ));
    }

    #[test]
    fn an_unknown_or_expired_ticket_is_refused_alike() {
        let (_dir, path) = store();
        let minted = mint_ticket(&path, scopes(), 3600, 600).unwrap();
        assert!(matches!(
            redeem_ticket(&path, &[0_u8; TICKET_BYTES]),
            Err(WorkloadError::NotFound(_))
        ));
        // Expire it in place.
        let mut file = load(&path).unwrap();
        file.tickets[0].expires_at = Utc::now().timestamp() - 1;
        store::with_lock(&path, |held| commit(&path, &file, held)).unwrap();
        let secret = hex::decode(&minted.secret_hex).unwrap();
        assert!(matches!(
            redeem_ticket(&path, &secret),
            Err(WorkloadError::NotFound(_))
        ));
    }

    #[test]
    fn concurrent_redemptions_of_one_ticket_admit_exactly_one() {
        let (_dir, path) = store();
        let minted = mint_ticket(&path, scopes(), 3600, 600).unwrap();
        let secret = hex::decode(&minted.secret_hex).unwrap();
        let winners = std::thread::scope(|scope| {
            #[allow(
                clippy::needless_collect,
                reason = "every redeemer is spawned before any is joined"
            )]
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| redeem_ticket(&path, &secret).is_ok()))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|won| *won)
                .count()
        });
        assert_eq!(winners, 1);
    }

    #[test]
    fn minting_refuses_bad_scopes_and_lifetimes() {
        let (_dir, path) = store();
        assert!(mint_ticket(&path, vec!["nope@global".to_owned()], 3600, 600).is_err());
        assert!(mint_ticket(&path, scopes(), 0, 600).is_err());
        assert!(mint_ticket(&path, scopes(), 3600, 0).is_err());
    }
}
