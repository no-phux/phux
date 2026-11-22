//! The process-wide sink the server records through.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{AuditActor, AuditLog, Entry, Kind, Outcome, Transport};
use serde_json::Value;

static LOG: Mutex<Option<AuditLog>> = Mutex::new(None);
static ENABLED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    /// A test's own log, so parallel tests never share the global sink.
    static TEST_LOG: std::cell::RefCell<Option<AuditLog>> = const { std::cell::RefCell::new(None) };
}

/// Where this profile's audit log lives.
#[must_use]
pub fn default_dir() -> PathBuf {
    crate::telemetry::state_dir().join("audit")
}

/// Start recording for this process (the server, once at startup). A
/// failure is logged and counted; the server runs without an audit log.
pub fn init(dir: &Path) {
    match AuditLog::open(dir) {
        Ok(log) => {
            *LOG.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(log);
            ENABLED.store(true, Ordering::Relaxed);
        }
        Err(error) => failed(&error),
    }
}

/// The server's recording: [`init`] on start, [`shutdown`] on drop. Start
/// it before any listener loads the workload registry, so the registry's
/// first snapshot is the credential baseline.
#[derive(Debug)]
pub struct Recording(());

impl Recording {
    /// Start recording to [`default_dir`].
    #[must_use]
    pub fn start() -> Self {
        init(&default_dir());
        Self(())
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        shutdown();
    }
}

/// Sync and close the log (server shutdown).
pub fn shutdown() {
    ENABLED.store(false, Ordering::Relaxed);
    let log = LOG
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(Err(error)) = log.as_ref().map(AuditLog::sync) {
        failed(&error);
    }
}

/// Whether records are being written; callers skip building one otherwise.
#[must_use]
pub fn enabled() -> bool {
    #[cfg(test)]
    if TEST_LOG.with(|log| log.borrow().is_some()) {
        return true;
    }
    ENABLED.load(Ordering::Relaxed)
}

/// Append `entry`, or count why not. Never fails the caller.
pub fn record(entry: Entry) {
    #[cfg(test)]
    let Some(entry) = TEST_LOG.with(|slot| match slot.borrow_mut().as_mut() {
        Some(log) => {
            if let Err(error) = log.append(entry) {
                failed(&error);
            }
            None
        }
        None => Some(entry),
    }) else {
        return;
    };
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let mut log = LOG
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(Err(error)) = log.as_mut().map(|log| log.append(entry)) {
        drop(log);
        failed(&error);
    }
}

/// Record a refused remote admission from `source`.
pub fn refused(transport: Transport, source: std::net::IpAddr, reason: &str) {
    emit(
        Kind::ConnectionRefused,
        AuditActor::anonymous(transport),
        "open",
        None,
        Outcome::Denied,
        serde_json::json!({ "source": source.to_string(), "reason": reason }),
    );
}

/// Active credentials per registry path, as last audited.
static REGISTRIES: Mutex<
    std::collections::BTreeMap<PathBuf, std::collections::BTreeMap<String, Vec<String>>>,
> = Mutex::new(std::collections::BTreeMap::new());

/// Audit how a valid registry snapshot differs from the last one seen at
/// `path`. The first snapshot per path is the baseline; several listeners
/// tracking one file report each change once.
#[allow(
    clippy::redundant_pub_crate,
    reason = "re-exported by `audit` to the crate"
)]
pub(crate) fn registry_loaded(path: &Path, registry: &crate::workload::WorkloadRegistry) {
    if !enabled() {
        return;
    }
    let now: std::collections::BTreeMap<String, Vec<String>> = registry
        .credentials()
        .iter()
        .filter(|credential| credential.revoked_at.is_none())
        .map(|credential| (credential.id.clone(), credential.scopes.clone()))
        .collect();
    let before = REGISTRIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_owned(), now.clone());
    let Some(before) = before else { return };
    let changes = now
        .iter()
        .filter(|(id, _)| !before.contains_key(*id))
        .map(|(id, scopes)| (Kind::CredentialAdded, id, scopes))
        .chain(
            before
                .iter()
                .filter(|(id, _)| !now.contains_key(*id))
                .map(|(id, scopes)| (Kind::CredentialRemoved, id, scopes)),
        );
    for (kind, id, scopes) in changes {
        emit(
            kind,
            AuditActor::server(),
            if kind == Kind::CredentialAdded {
                "add"
            } else {
                "remove"
            },
            Some(id.clone()),
            Outcome::Allowed,
            serde_json::json!({ "scopes": scopes }),
        );
    }
}

/// Shorthand for [`record`].
pub fn emit(
    kind: Kind,
    actor: AuditActor,
    action: impl Into<String>,
    target: Option<String>,
    outcome: Outcome,
    detail: Value,
) {
    if enabled() {
        record(Entry {
            kind,
            actor,
            action: action.into(),
            target,
            outcome,
            detail,
        });
    }
}

fn failed(error: &io::Error) {
    crate::perf::AUDIT_WRITE_FAILURES.incr();
    let count = crate::perf::AUDIT_WRITE_FAILURES.get();
    // The first failure, then every power of two: a full disk must not
    // flood the server log in turn.
    if count.is_power_of_two() {
        tracing::warn!(%error, failures = count, "audit record not written");
    }
}

/// Route this thread's records to a log in `dir` (tests only).
#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "re-exported by `audit` to the crate"
)]
pub(crate) fn capture_for_test(dir: &Path, rotate_at: u64) {
    let mut log = AuditLog::open(dir).expect("open test audit log");
    log.rotate_at = rotate_at;
    TEST_LOG.with(|slot| *slot.borrow_mut() = Some(log));
}

/// Stop routing this thread's records (tests only).
#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "re-exported by `audit` to the crate"
)]
pub(crate) fn release_test_capture() {
    TEST_LOG.with(|slot| slot.borrow_mut().take());
}
