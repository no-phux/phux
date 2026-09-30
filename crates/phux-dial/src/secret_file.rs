//! Ownership and mode checks for the secret files the TLS-terminating crates
//! read: a listener's private key, the relay's route-token store, and a relay
//! connector's token file.
//!
//! A secret another account can replace hands that account the listener (a
//! swapped key or token), and one it can read hands it the credential. Each
//! [`SecretFile`] kind says which of those it refuses outright and which it
//! only warns about, so an operator setup that already works (a key readable
//! by an `ssl-cert` group, a hand-edited token store) keeps working, loudly.

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

/// What kind of secret a file holds, which sets how strictly it is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretFile {
    /// A TLS private key. Refused when another account (other than root)
    /// owns it, when others can read it, or when anyone but the owner can
    /// write it; readable by the group only warns.
    PrivateKey,
    /// The relay's route-token store. Refused when another account owns it
    /// or anyone but the owner can write it; readable by others only warns.
    TokenStore,
    /// A single owner-only token file. Refused when another account owns it
    /// or it carries any group or other permission.
    OwnerOnlyToken,
}

impl SecretFile {
    /// Mode bits that refuse the file.
    const fn refused_bits(self) -> u32 {
        match self {
            Self::PrivateKey => 0o026,
            Self::TokenStore => 0o022,
            Self::OwnerOnlyToken => 0o077,
        }
    }

    /// Mode bits that only warn.
    const fn warned_bits(self) -> u32 {
        match self {
            Self::PrivateKey => 0o040,
            Self::TokenStore => 0o044,
            Self::OwnerOnlyToken => 0,
        }
    }

    /// Whether a root-owned file is accepted from a non-root process: an
    /// operator's key under `/etc` is root's; phux's own stores never are.
    const fn admits_root_owner(self) -> bool {
        matches!(self, Self::PrivateKey)
    }

    const fn noun(self) -> &'static str {
        match self {
            Self::PrivateKey => "TLS private key",
            Self::TokenStore => "token store",
            Self::OwnerOnlyToken => "token file",
        }
    }
}

/// Check `path`'s `metadata` as `kind` for a process running as `euid`.
///
/// The metadata is the file's own (symlinks followed). `Ok(Some(warning))`
/// admits the file but names a loosening the operator should fix.
///
/// # Errors
///
/// The refusal, naming the file, what is wrong, and the fix.
pub fn check_metadata(
    path: &Path,
    metadata: &Metadata,
    kind: SecretFile,
    euid: u32,
) -> Result<Option<String>, String> {
    let owner = metadata.uid();
    if owner != euid && !(owner == 0 && kind.admits_root_owner()) {
        return Err(format!(
            "{} {} is owned by uid {owner}, not this process's uid {euid}; \
             chown it to {euid} or run as its owner",
            kind.noun(),
            path.display()
        ));
    }
    let mode = metadata.mode() & 0o777;
    if mode & kind.refused_bits() != 0 {
        return Err(format!(
            "{} {} has mode {mode:04o}, which lets other accounts {}; chmod 600 it",
            kind.noun(),
            path.display(),
            if mode & 0o022 != 0 {
                "replace it"
            } else {
                "read it"
            }
        ));
    }
    if mode & kind.warned_bits() != 0 {
        return Ok(Some(format!(
            "{} {} has mode {mode:04o}, readable by other accounts; chmod 600 it",
            kind.noun(),
            path.display()
        )));
    }
    Ok(None)
}

/// [`check_metadata`] for the file at `path`, as this process's effective
/// uid.
///
/// # Errors
///
/// The refusal, or the `stat` failure, as a message.
pub fn check(path: &Path, kind: SecretFile) -> Result<Option<String>, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("{} {}: {error}", kind.noun(), path.display()))?;
    check_metadata(path, &metadata, kind, effective_uid())
}

/// This process's effective uid.
#[must_use]
pub fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn file_with_mode(dir: &Path, mode: u32) -> std::path::PathBuf {
        let path = dir.join(format!("secret-{mode:o}"));
        std::fs::write(&path, b"secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    /// Each kind's refused, warned, and accepted modes, for the owner.
    #[test]
    fn modes_refuse_warn_or_admit_per_kind() {
        let dir = tempfile::tempdir().unwrap();
        let me = effective_uid();
        for (kind, mode, want) in [
            (SecretFile::PrivateKey, 0o600, "ok"),
            (SecretFile::PrivateKey, 0o400, "ok"),
            (SecretFile::PrivateKey, 0o640, "warn"),
            (SecretFile::PrivateKey, 0o644, "refuse"),
            (SecretFile::PrivateKey, 0o620, "refuse"),
            (SecretFile::TokenStore, 0o600, "ok"),
            (SecretFile::TokenStore, 0o644, "warn"),
            (SecretFile::TokenStore, 0o664, "refuse"),
            (SecretFile::TokenStore, 0o606, "refuse"),
            (SecretFile::OwnerOnlyToken, 0o600, "ok"),
            (SecretFile::OwnerOnlyToken, 0o640, "refuse"),
        ] {
            let path = file_with_mode(dir.path(), mode);
            let metadata = std::fs::metadata(&path).unwrap();
            let got = match check_metadata(&path, &metadata, kind, me) {
                Ok(None) => "ok",
                Ok(Some(_)) => "warn",
                Err(_) => "refuse",
            };
            assert_eq!(got, want, "{kind:?} {mode:04o}");
        }
    }

    /// A file another account owns is refused; root's key is admitted.
    #[test]
    fn foreign_owners_are_refused_except_root_for_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with_mode(dir.path(), 0o600);
        let metadata = std::fs::metadata(&path).unwrap();
        let someone_else = metadata.uid().wrapping_add(1).max(1);
        for kind in [
            SecretFile::PrivateKey,
            SecretFile::TokenStore,
            SecretFile::OwnerOnlyToken,
        ] {
            let error = check_metadata(&path, &metadata, kind, someone_else).unwrap_err();
            assert!(error.contains("owned by uid"), "{error}");
        }
        if metadata.uid() == 0 {
            return;
        }
        // Seen from root's side, the owner is foreign; the key check admits
        // only a root owner, never a root reader.
        assert!(check_metadata(&path, &metadata, SecretFile::PrivateKey, 0).is_err());
    }
}
