//! Machine-local approval of repository recipes (ADR-0152).
//!
//! An approval names three things: the repository's identity (its git
//! common directory, so every worktree of one repository shares it, or the
//! directory itself outside git), the recipe's path relative to the
//! checkout, and the SHA-256 of its exact bytes. Changing one byte makes the
//! recipe untrusted again, and the same bytes in another repository are not
//! trusted: a recipe may run repository-relative programs.
//!
//! The store is `<state dir>/trusted-projects.toml`, owner-only, rewritten
//! atomically under an advisory lock. A store that cannot be read or parsed
//! fails closed: nothing is trusted.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const STORE_FILE: &str = "trusted-projects.toml";
const LOCK_FILE: &str = "trusted-projects.lock";
const STORE_SCHEMA: u8 = 1;
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;

/// What one approval is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrustKey {
    /// The repository identity (canonical git common dir, or the root).
    pub(crate) identity: PathBuf,
    /// The recipe path relative to the checkout root.
    pub(crate) recipe: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    schema: u8,
    #[serde(default)]
    recipes: Vec<Approval>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Approval {
    identity: PathBuf,
    recipe: PathBuf,
    sha256: String,
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn digest(bytes: &[u8]) -> String {
    crate::commands::update::apply::hex_lower(&Sha256::digest(bytes))
}

/// The store in phux's state directory.
pub(crate) fn default_dir() -> PathBuf {
    phux_config::instance::state_dir()
}

/// Whether `sha256` is the approved digest for `key`.
///
/// # Errors
///
/// The store exists but cannot be read or parsed (fail closed).
pub(crate) fn is_trusted(dir: &Path, key: &TrustKey, sha256: &str) -> Result<bool, String> {
    let store = read_store(&dir.join(STORE_FILE))?;
    Ok(store.recipes.iter().any(|approval| {
        approval.identity == key.identity
            && approval.recipe == key.recipe
            && approval.sha256 == sha256
    }))
}

/// Approve exactly `sha256` for `key`, replacing an earlier approval.
///
/// # Errors
///
/// The store cannot be locked, read, or rewritten.
pub(crate) fn trust(dir: &Path, key: &TrustKey, sha256: &str) -> Result<(), String> {
    update(dir, |store| {
        store.recipes.retain(|approval| !same_key(approval, key));
        store.recipes.push(Approval {
            identity: key.identity.clone(),
            recipe: key.recipe.clone(),
            sha256: sha256.to_owned(),
        });
        true
    })
    .map(|_| ())
}

/// Withdraw the approval for `key`; `Ok(false)` when there was none.
///
/// # Errors
///
/// The store cannot be locked, read, or rewritten.
pub(crate) fn untrust(dir: &Path, key: &TrustKey) -> Result<bool, String> {
    update(dir, |store| {
        let before = store.recipes.len();
        store.recipes.retain(|approval| !same_key(approval, key));
        store.recipes.len() != before
    })
}

fn same_key(approval: &Approval, key: &TrustKey) -> bool {
    approval.identity == key.identity && approval.recipe == key.recipe
}

/// Read-modify-write under the lock; `change` reports whether to write.
fn update(dir: &Path, change: impl FnOnce(&mut Store) -> bool) -> Result<bool, String> {
    create_private_dir(dir)?;
    let _lock = lock(&dir.join(LOCK_FILE))?;
    let path = dir.join(STORE_FILE);
    let mut store = read_store(&path)?;
    if !change(&mut store) {
        return Ok(false);
    }
    store.schema = STORE_SCHEMA;
    let text =
        toml::to_string(&store).map_err(|err| format!("could not encode trust store: {err}"))?;
    write_private(&path, text.as_bytes())?;
    Ok(true)
}

fn read_store(path: &Path) -> Result<Store, String> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Store {
                schema: STORE_SCHEMA,
                recipes: Vec::new(),
            });
        }
        Err(err) => {
            return Err(format!(
                "could not read trust store {}: {err}",
                path.display()
            ));
        }
    };
    let meta = file
        .metadata()
        .map_err(|err| format!("could not inspect trust store {}: {err}", path.display()))?;
    if !meta.is_file() || meta.len() > MAX_STORE_BYTES {
        return Err(format!(
            "trust store {} is not a regular file under {MAX_STORE_BYTES} bytes",
            path.display()
        ));
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut &file, &mut text)
        .map_err(|err| format!("could not read trust store {}: {err}", path.display()))?;
    let store: Store = toml::from_str(&text)
        .map_err(|err| format!("trust store {} is malformed: {err}", path.display()))?;
    if store.schema != STORE_SCHEMA {
        return Err(format!(
            "trust store {} has schema {}; this phux reads {STORE_SCHEMA}",
            path.display(),
            store.schema
        ));
    }
    Ok(store)
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| format!("could not create {}: {err}", dir.display()))
}

/// An exclusive advisory lock held until the returned file drops.
fn lock(path: &Path) -> Result<fs::File, String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|err| format!("could not open {}: {err}", path.display()))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|err| format!("could not lock {}: {err}", path.display()))?;
    Ok(file)
}

/// Replace `path` with `bytes`, mode 0600, via a synced sibling and rename.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| format!("could not create {}: {err}", tmp.display()))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|err| format!("could not chmod {}: {err}", tmp.display()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
        fs::rename(&tmp, path).map_err(|err| format!("could not replace {}: {err}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn key(identity: &str) -> TrustKey {
        TrustKey {
            identity: PathBuf::from(identity),
            recipe: PathBuf::from(".phux/project.toml"),
        }
    }

    #[test]
    fn approval_is_exact_bytes_per_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dir.path().join("state");
        let a = digest(b"one");
        let b = digest(b"two");
        assert!(!is_trusted(&store, &key("/repo"), &a).expect("empty store reads"));
        trust(&store, &key("/repo"), &a).expect("trust");
        assert!(is_trusted(&store, &key("/repo"), &a).expect("read"));
        assert!(
            !is_trusted(&store, &key("/repo"), &b).expect("read"),
            "changed bytes"
        );
        assert!(
            !is_trusted(&store, &key("/other"), &a).expect("read"),
            "other repo"
        );
        trust(&store, &key("/repo"), &b).expect("re-trust");
        assert!(
            !is_trusted(&store, &key("/repo"), &a).expect("read"),
            "replaced"
        );
        assert!(untrust(&store, &key("/repo")).expect("untrust"));
        assert!(!untrust(&store, &key("/repo")).expect("idempotent"));
        assert!(!is_trusted(&store, &key("/repo"), &b).expect("read"));
        let mode = fs::metadata(store.join(STORE_FILE))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_malformed_store_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(STORE_FILE), "not = [valid").expect("write");
        assert!(is_trusted(dir.path(), &key("/repo"), &digest(b"x")).is_err());
        assert!(trust(dir.path(), &key("/repo"), &digest(b"x")).is_err());
    }
}
