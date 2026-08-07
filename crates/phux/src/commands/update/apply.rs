//! Verify, unpack, and replace — the half of `phux update` that touches the
//! filesystem.
//!
//! Three invariants hold this module together, in order:
//!
//! 1. **The checksum gates everything.** The `.sha256` sidecar is compared
//!    against a digest computed here, over the file on disk, *before* `tar`
//!    is ever pointed at the archive. A mismatch is a hard refusal that names
//!    both digests; nothing is unpacked and nothing is replaced.
//! 2. **Nothing downloaded is executed to decide whether to install it.** The
//!    archive is data: its member list is validated, it is unpacked, and the
//!    extracted tree is validated again. The one place a new binary *is* run
//!    is the server's own pre-commit `--version` check in
//!    `phux-server/src/runtime/upgrade.rs`, which happens after installation,
//!    on the server side, where a failure is harmless because nothing has
//!    been closed yet.
//! 3. **The destination path never holds a partial file.** Staging happens in
//!    a sibling directory of the target — same directory, therefore same
//!    filesystem, therefore `rename(2)` is atomic — and the final step is a
//!    rename over the destination. A crash at any point leaves either the old
//!    binary or the new one, never half of either.
//!
//! Permissions come from the file being replaced, not from the archive. That
//! preserves a deliberately restrictive mode (a `0o700` binary in a shared
//! bin directory stays `0o700`) and, in the other direction, means a setuid
//! bit smuggled into a tarball cannot survive the replacement.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::UpdateError;

/// The binaries a phux release tarball ships, in replacement order.
///
/// `phux-mcp` goes first and `phux` last on purpose: the pair is replaced by
/// two renames and two renames cannot be one atomic step, so the primary
/// binary is the last thing to move. If the second rename fails, the first is
/// rolled back and the install is left entirely on the old pair.
pub(crate) const RELEASE_BINARIES: &[&str] = &["phux-mcp", "phux"];

/// Non-executable members every release tarball also carries.
const RELEASE_DOCS: &[&str] = &["README.md", "LICENSE-MIT", "LICENSE-APACHE"];

/// The directory, inside the install's bin directory, that holds the previous
/// binaries after a successful update. Same directory means same filesystem,
/// which is what lets a rollback be a rename rather than a copy.
pub(crate) const BACKUP_DIR: &str = ".phux-update-backup";

/// The manifest written beside the saved binaries.
const BACKUP_MANIFEST: &str = "manifest.json";

/// Compute the SHA-256 of a file as lowercase hex.
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Lowercase hex, without pulling in a dependency for sixteen characters.
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        // Writing into a String cannot fail.
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Read the expected digest out of a `.sha256` sidecar.
///
/// The sidecar `release.yml` writes is one line, `"<64 hex>  <archive>"` —
/// the `shasum`/`sha256sum` interchange format. The name is checked as well
/// as the digest: a sidecar that names a different artifact means the release
/// assets are crossed, which is exactly the kind of quiet mismatch a checksum
/// exists to catch.
pub(crate) fn expected_digest(sidecar: &str, archive: &str) -> Result<String, UpdateError> {
    let line = sidecar
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| UpdateError::Checksum("the .sha256 sidecar was empty".to_owned()))?;
    let mut fields = line.split_whitespace();
    let digest = fields
        .next()
        .ok_or_else(|| UpdateError::Checksum("the .sha256 sidecar had no digest".to_owned()))?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(UpdateError::Checksum(format!(
            "the .sha256 sidecar did not start with a 64-character hex digest (got `{digest}`)"
        )));
    }
    // `sha256sum -b` prefixes the name with `*`; `shasum -a 256` does not.
    if let Some(named) = fields.next() {
        let named = named.strip_prefix('*').unwrap_or(named);
        if named != archive {
            return Err(UpdateError::Checksum(format!(
                "the .sha256 sidecar is for `{named}`, not `{archive}`"
            )));
        }
    }
    Ok(digest.to_ascii_lowercase())
}

/// Verify `archive` against `sidecar`, returning the digest both agree on.
///
/// This is the trust anchor. Everything downstream — unpacking, staging,
/// replacement — runs only if this returns `Ok`.
pub(crate) fn verify_archive(
    archive: &Path,
    sidecar: &str,
    archive_name: &str,
) -> Result<String, UpdateError> {
    let expected = expected_digest(sidecar, archive_name)?;
    let actual = sha256_file(archive)
        .map_err(|err| UpdateError::Checksum(format!("could not hash the download: {err}")))?;
    if actual == expected {
        Ok(actual)
    } else {
        Err(UpdateError::ChecksumMismatch {
            expected,
            actual,
            archive: archive_name.to_owned(),
        })
    }
}

/// The exact set of members a `phux-<tag>-<target>.tar.gz` may contain.
fn allowed_members(stage: &str) -> BTreeSet<String> {
    let mut allowed = BTreeSet::new();
    allowed.insert(format!("{stage}/"));
    allowed.insert(stage.to_owned());
    for name in RELEASE_BINARIES.iter().chain(RELEASE_DOCS) {
        allowed.insert(format!("{stage}/{name}"));
    }
    allowed
}

/// Unpack a **verified** archive into `into`, returning the staged directory.
///
/// The member list is checked before extraction and the extracted tree is
/// checked after it. The two checks are not redundant: the first refuses an
/// archive whose table of contents names anything unexpected (absolute paths,
/// `..`, extra files), the second catches anything that reached the disk in a
/// shape the listing did not describe — a symlink, a hard link, a device
/// node.
pub(crate) fn unpack_verified(
    archive: &Path,
    stage: &str,
    into: &Path,
) -> Result<PathBuf, UpdateError> {
    let listing = tar(&["-tzf", &archive.to_string_lossy()])?;
    let allowed = allowed_members(stage);
    let mut saw_binary = false;
    for member in listing.lines().map(str::trim).filter(|m| !m.is_empty()) {
        if !allowed.contains(member) {
            return Err(UpdateError::Archive(format!(
                "unexpected member `{member}` in {}",
                archive.display()
            )));
        }
        if member == format!("{stage}/phux") {
            saw_binary = true;
        }
    }
    if !saw_binary {
        return Err(UpdateError::Archive(format!(
            "{} does not contain {stage}/phux",
            archive.display()
        )));
    }

    tar(&[
        "-xzf",
        &archive.to_string_lossy(),
        "-C",
        &into.to_string_lossy(),
    ])?;

    let staged = into.join(stage);
    validate_extracted(&staged, stage)?;
    Ok(staged)
}

/// Check the tree `tar` actually produced.
fn validate_extracted(staged: &Path, stage: &str) -> Result<(), UpdateError> {
    let meta = fs::symlink_metadata(staged).map_err(|err| {
        UpdateError::Archive(format!("{} is unreadable: {err}", staged.display()))
    })?;
    if !meta.is_dir() {
        return Err(UpdateError::Archive(format!(
            "{} is not a directory",
            staged.display()
        )));
    }
    let entries = fs::read_dir(staged).map_err(|err| {
        UpdateError::Archive(format!("{} is unreadable: {err}", staged.display()))
    })?;
    let allowed = allowed_members(stage);
    for entry in entries {
        let entry =
            entry.map_err(|err| UpdateError::Archive(format!("unpack directory error: {err}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !allowed.contains(&format!("{stage}/{name}")) {
            return Err(UpdateError::Archive(format!(
                "unexpected extracted member `{name}`"
            )));
        }
        let meta = fs::symlink_metadata(entry.path()).map_err(|err| {
            UpdateError::Archive(format!("{} is unreadable: {err}", entry.path().display()))
        })?;
        if meta.file_type().is_symlink() {
            return Err(UpdateError::Archive(format!(
                "extracted member `{name}` is a symlink"
            )));
        }
        if !meta.is_file() {
            return Err(UpdateError::Archive(format!(
                "extracted member `{name}` is not a regular file"
            )));
        }
    }
    Ok(())
}

/// Run `tar` with `args`.
fn tar(args: &[&str]) -> Result<String, UpdateError> {
    let output = Command::new("tar")
        .args(args)
        .output()
        .map_err(|err| UpdateError::Archive(format!("could not run `tar`: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(UpdateError::Archive(format!(
            "`tar` exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A scratch directory inside the target's own directory, removed on drop.
///
/// Being a sibling of the target is not a convenience: it is what guarantees
/// the staged file and the destination share a filesystem, which is what
/// makes the final `rename` atomic. A staging directory in `/tmp` would force
/// a cross-device copy into place, and a copy is exactly the window this
/// module exists to close.
#[derive(Debug)]
pub(crate) struct Staging {
    path: PathBuf,
}

impl Staging {
    /// Create a fresh staging directory beside the binaries in `bin_dir`.
    pub(crate) fn create(bin_dir: &Path) -> Result<Self, UpdateError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let path = bin_dir.join(format!(".phux-update-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).map_err(|err| {
            UpdateError::Install(format!(
                "could not create a staging directory at {}: {err}\n\
                 {} must be writable by this user for phux to update in place",
                path.display(),
                bin_dir.display()
            ))
        })?;
        Ok(Self { path })
    }

    /// The staging directory.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // Best effort: a leftover dot-directory is untidy, not dangerous, and
        // there is nothing useful to report from a destructor.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The outcome of a successful replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replaced {
    /// The binaries that were replaced, in the order they moved.
    pub(crate) binaries: Vec<String>,
    /// Where the previous binaries were saved.
    pub(crate) backup: PathBuf,
    /// The version the saved binaries are.
    pub(crate) previous_version: String,
}

/// Atomically replace the release binaries in `bin_dir` from `staged`.
///
/// `phux-mcp` is replaced alongside `phux` when it is present beside it. That
/// is not tidiness: ADR-0071 makes the *release* the compatibility unit, so a
/// new `phux` next to a stale `phux-mcp` is precisely the mismatched-peer
/// state the update path exists to prevent.
pub(crate) fn replace_binaries(
    bin_dir: &Path,
    staged: &Path,
    previous_version: &str,
) -> Result<Replaced, UpdateError> {
    // Only replace what is actually installed here. `phux` itself is
    // mandatory; `phux-mcp` is replaced when it is already a sibling.
    let targets: Vec<&str> = RELEASE_BINARIES
        .iter()
        .copied()
        .filter(|name| *name == "phux" || bin_dir.join(name).exists())
        .collect();

    let backup = prepare_backup(bin_dir, &targets, previous_version)?;

    let mut moved: Vec<String> = Vec::new();
    for name in &targets {
        let from = staged.join(name);
        let to = bin_dir.join(name);
        if let Err(err) = adopt_permissions(&from, &to) {
            undo(bin_dir, &backup, &moved);
            return Err(err);
        }
        if let Err(err) = fs::rename(&from, &to) {
            let failure =
                UpdateError::Install(format!("could not install {}: {err}", to.display()));
            undo(bin_dir, &backup, &moved);
            return Err(failure);
        }
        moved.push((*name).to_owned());
    }

    Ok(Replaced {
        binaries: moved,
        backup,
        previous_version: previous_version.to_owned(),
    })
}

/// Give the staged file the mode of the file it is about to replace.
///
/// When there is nothing to replace (a `phux-mcp` that was never installed —
/// which `replace_binaries` filters out, but the helper stays honest anyway)
/// the archive's own executable bit is kept and narrowed to `0o755`.
fn adopt_permissions(staged: &Path, target: &Path) -> Result<(), UpdateError> {
    let mode = fs::metadata(target).map_or(0o755, |meta| meta.permissions().mode());
    fs::set_permissions(staged, fs::Permissions::from_mode(mode)).map_err(|err| {
        UpdateError::Install(format!(
            "could not set mode {:o} on {}: {err}",
            mode & 0o7777,
            staged.display()
        ))
    })
}

/// Save the current binaries into `bin_dir/.phux-update-backup`.
fn prepare_backup(
    bin_dir: &Path,
    targets: &[&str],
    previous_version: &str,
) -> Result<PathBuf, UpdateError> {
    let backup = bin_dir.join(BACKUP_DIR);
    // A stale backup from an earlier update is not useful — its binaries are
    // two versions behind whatever is live — and keeping it would make the
    // manifest lie about what rolling back restores.
    if backup.exists() {
        fs::remove_dir_all(&backup).map_err(|err| {
            UpdateError::Install(format!(
                "could not clear the previous backup at {}: {err}",
                backup.display()
            ))
        })?;
    }
    fs::create_dir(&backup).map_err(|err| {
        UpdateError::Install(format!("could not create {}: {err}", backup.display()))
    })?;

    let mut saved = Vec::new();
    for name in targets {
        let live = bin_dir.join(name);
        if !live.exists() {
            continue;
        }
        let into = backup.join(name);
        // A hard link is free and keeps the exact inode — including its mode
        // — so a rollback restores byte-for-byte what was there. `fs::copy`
        // is the fallback for filesystems that refuse links.
        if fs::hard_link(&live, &into).is_err() {
            fs::copy(&live, &into).map_err(|err| {
                UpdateError::Install(format!(
                    "could not save {} before replacing it: {err}",
                    live.display()
                ))
            })?;
        }
        saved.push((*name).to_owned());
    }

    let manifest = serde_json::json!({
        "schema_version": 1,
        "version": previous_version,
        "binaries": saved,
        "saved_at": chrono::Utc::now().to_rfc3339(),
    });
    let rendered = serde_json::to_string_pretty(&manifest).map_err(|err| {
        UpdateError::Install(format!("could not render the backup manifest: {err}"))
    })?;
    fs::write(backup.join(BACKUP_MANIFEST), rendered).map_err(|err| {
        UpdateError::Install(format!(
            "could not write {}: {err}",
            backup.join(BACKUP_MANIFEST).display()
        ))
    })?;
    Ok(backup)
}

/// Put back everything that already moved, after a mid-pair failure.
///
/// Best effort by necessity — if this also fails the user is told to run
/// `phux update --rollback`, and the saved binaries are still on disk for it
/// to find.
fn undo(bin_dir: &Path, backup: &Path, moved: &[String]) {
    for name in moved {
        let _ = fs::rename(backup.join(name), bin_dir.join(name));
    }
}

/// What a rollback restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RolledBack {
    /// The binaries put back.
    pub(crate) binaries: Vec<String>,
    /// The version they are.
    pub(crate) version: String,
}

/// Restore the binaries saved by the last successful update.
///
/// The restore is renames within one directory, so each binary is swapped
/// atomically and the backup directory is removed only once every one of them
/// is back. A rollback with nothing saved is an error, not a silent no-op.
pub(crate) fn rollback(bin_dir: &Path) -> Result<RolledBack, UpdateError> {
    let backup = bin_dir.join(BACKUP_DIR);
    let manifest_path = backup.join(BACKUP_MANIFEST);
    let raw = fs::read_to_string(&manifest_path).map_err(|err| {
        UpdateError::NoBackup(format!("no saved binaries at {}: {err}", backup.display()))
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        UpdateError::NoBackup(format!(
            "{} is not readable: {err}",
            manifest_path.display()
        ))
    })?;
    let version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let names: Vec<String> = manifest
        .get("binaries")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(std::borrow::ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        return Err(UpdateError::NoBackup(format!(
            "{} lists no saved binaries",
            manifest_path.display()
        )));
    }

    // Refuse before moving anything if any saved file is missing, so a
    // rollback is all-or-nothing rather than half-applied.
    for name in &names {
        let saved = backup.join(name);
        if !saved.exists() {
            return Err(UpdateError::NoBackup(format!(
                "{} is missing; the backup is incomplete and was not applied",
                saved.display()
            )));
        }
    }

    let mut restored = Vec::new();
    for name in &names {
        let saved = backup.join(name);
        let live = bin_dir.join(name);
        fs::rename(&saved, &live).map_err(|err| {
            UpdateError::Install(format!(
                "could not restore {} from {}: {err}\n\
                 {} restored so far; the rest are still in {}",
                live.display(),
                saved.display(),
                restored.join(", "),
                backup.display()
            ))
        })?;
        restored.push(name.clone());
    }

    let _ = fs::remove_dir_all(&backup);
    Ok(RolledBack {
        binaries: restored,
        version,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::{
        BACKUP_DIR, Staging, UpdateError, expected_digest, replace_binaries, rollback, sha256_file,
        unpack_verified, verify_archive,
    };

    /// A private scratch directory, removed when the guard drops.
    #[derive(Debug)]
    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "phux-update-test-{tag}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// The SHA-256 of the empty input, the standard published vector.
    #[test]
    fn sha256_matches_the_published_vector() {
        let scratch = Scratch::new("sha");
        let empty = scratch.path().join("empty");
        fs::write(&empty, b"").unwrap();
        assert_eq!(
            sha256_file(&empty).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let abc = scratch.path().join("abc");
        fs::write(&abc, b"abc").unwrap();
        assert_eq!(
            sha256_file(&abc).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sidecar_parsing_accepts_the_release_workflow_format() {
        let digest = "a".repeat(64);
        let sidecar = format!("{digest}  phux-v1.0.0-x.tar.gz\n");
        assert_eq!(
            expected_digest(&sidecar, "phux-v1.0.0-x.tar.gz").unwrap(),
            digest
        );
        // `sha256sum -b`'s binary marker.
        let starred = format!("{digest} *phux-v1.0.0-x.tar.gz\n");
        assert_eq!(
            expected_digest(&starred, "phux-v1.0.0-x.tar.gz").unwrap(),
            digest
        );
        // Digest-only sidecars are accepted; there is nothing to cross-check.
        assert_eq!(
            expected_digest(&digest, "phux-v1.0.0-x.tar.gz").unwrap(),
            digest
        );
    }

    #[test]
    fn sidecar_parsing_refuses_malformed_and_crossed_sidecars() {
        let digest = "b".repeat(64);
        for (sidecar, why) in [
            (String::new(), "empty"),
            ("not-a-digest  phux-v1.0.0-x.tar.gz".to_owned(), "short"),
            (format!("{}  x.tar.gz", "z".repeat(64)), "non-hex"),
            (
                format!("{digest}  phux-v9.9.9-other.tar.gz"),
                "names another artifact",
            ),
        ] {
            assert!(
                expected_digest(&sidecar, "phux-v1.0.0-x.tar.gz").is_err(),
                "sidecar should be refused ({why})"
            );
        }
    }

    /// The gate: a tampered archive is refused, loudly, with both digests.
    #[test]
    fn a_checksum_mismatch_refuses_and_names_both_digests() {
        let scratch = Scratch::new("mismatch");
        let archive = scratch.path().join("phux-v1.0.0-t.tar.gz");
        fs::write(&archive, b"the real bytes").unwrap();
        let good = sha256_file(&archive).unwrap();

        // Matching sidecar: accepted, and the digest is handed back.
        let sidecar = format!("{good}  phux-v1.0.0-t.tar.gz\n");
        assert_eq!(
            verify_archive(&archive, &sidecar, "phux-v1.0.0-t.tar.gz").unwrap(),
            good
        );

        // Someone swapped the archive after the sidecar was published.
        fs::write(&archive, b"tampered bytes").unwrap();
        let err = verify_archive(&archive, &sidecar, "phux-v1.0.0-t.tar.gz").unwrap_err();
        match err {
            UpdateError::ChecksumMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, good);
                assert_ne!(actual, good);
            }
            other => panic!("expected a checksum mismatch, got {other:?}"),
        }
    }

    /// Build a release-shaped tarball in `dir`, returning its path.
    fn build_archive(dir: &Path, stage: &str, extra: Option<(&str, &[u8])>) -> PathBuf {
        let staging = dir.join("build").join(stage);
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("phux"), b"#!/bin/sh\nnew phux\n").unwrap();
        fs::set_permissions(staging.join("phux"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(staging.join("phux-mcp"), b"#!/bin/sh\nnew mcp\n").unwrap();
        fs::set_permissions(staging.join("phux-mcp"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(staging.join("README.md"), b"readme").unwrap();
        fs::write(staging.join("LICENSE-MIT"), b"mit").unwrap();
        fs::write(staging.join("LICENSE-APACHE"), b"apache").unwrap();
        if let Some((name, bytes)) = extra {
            fs::write(staging.join(name), bytes).unwrap();
        }
        let archive = dir.join(format!("{stage}.tar.gz"));
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.join("build"))
            .arg(stage)
            .status()
            .unwrap();
        assert!(status.success());
        archive
    }

    #[test]
    fn a_release_shaped_archive_unpacks_and_an_unexpected_member_is_refused() {
        let scratch = Scratch::new("unpack");
        let stage = "phux-v1.0.0-aarch64-apple-darwin";

        let archive = build_archive(scratch.path(), stage, None);
        let into = scratch.path().join("into");
        fs::create_dir(&into).unwrap();
        let staged = unpack_verified(&archive, stage, &into).unwrap();
        assert!(staged.join("phux").is_file());
        assert!(staged.join("phux-mcp").is_file());

        let hostile = Scratch::new("unpack-hostile");
        let archive = build_archive(hostile.path(), stage, Some(("payload.sh", b"rm -rf /")));
        let into = hostile.path().join("into");
        fs::create_dir(&into).unwrap();
        let err = unpack_verified(&archive, stage, &into).unwrap_err();
        assert!(
            format!("{err}").contains("payload.sh"),
            "the refusal must name the unexpected member: {err}"
        );
        assert!(
            !into.join(stage).exists(),
            "nothing may be extracted when the listing is refused"
        );
    }

    /// Seed a bin directory with a "current" install at a chosen mode.
    fn seed_bin_dir(dir: &Path, mode: u32) {
        fs::write(dir.join("phux"), b"#!/bin/sh\nold phux\n").unwrap();
        fs::set_permissions(dir.join("phux"), fs::Permissions::from_mode(mode)).unwrap();
        fs::write(dir.join("phux-mcp"), b"#!/bin/sh\nold mcp\n").unwrap();
        fs::set_permissions(dir.join("phux-mcp"), fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn replacement_is_atomic_preserves_permissions_and_is_reversible() {
        let scratch = Scratch::new("replace");
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        // A deliberately restrictive mode: the update must not widen it.
        seed_bin_dir(&bin, 0o700);

        let stage = "phux-v1.0.0-aarch64-apple-darwin";
        let archive = build_archive(scratch.path(), stage, None);
        let staging = Staging::create(&bin).unwrap();
        let staged = unpack_verified(&archive, stage, staging.path()).unwrap();

        let replaced = replace_binaries(&bin, &staged, "0.12.1").unwrap();
        assert_eq!(replaced.binaries, vec!["phux-mcp", "phux"]);
        assert_eq!(replaced.previous_version, "0.12.1");
        assert_eq!(replaced.backup, bin.join(BACKUP_DIR));

        // The new bytes are live...
        assert_eq!(
            fs::read(bin.join("phux")).unwrap(),
            b"#!/bin/sh\nnew phux\n"
        );
        assert_eq!(
            fs::read(bin.join("phux-mcp")).unwrap(),
            b"#!/bin/sh\nnew mcp\n"
        );
        // ...at the mode the old file carried, not the archive's 0o755.
        for name in ["phux", "phux-mcp"] {
            let mode = fs::metadata(bin.join(name)).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode, 0o700, "{name} kept the wrong mode");
        }
        // No staging or partial file is reachable at the destination.
        assert!(!bin.join("phux.new").exists());

        // Rollback puts the previous pair back and clears the backup.
        let back = rollback(&bin).unwrap();
        assert_eq!(back.version, "0.12.1");
        assert_eq!(
            fs::read(bin.join("phux")).unwrap(),
            b"#!/bin/sh\nold phux\n"
        );
        assert_eq!(
            fs::read(bin.join("phux-mcp")).unwrap(),
            b"#!/bin/sh\nold mcp\n"
        );
        assert!(!bin.join(BACKUP_DIR).exists());
    }

    #[test]
    fn a_lone_phux_install_does_not_grow_a_phux_mcp() {
        let scratch = Scratch::new("lone");
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("phux"), b"old").unwrap();
        fs::set_permissions(bin.join("phux"), fs::Permissions::from_mode(0o755)).unwrap();

        let stage = "phux-v1.0.0-aarch64-apple-darwin";
        let archive = build_archive(scratch.path(), stage, None);
        let staging = Staging::create(&bin).unwrap();
        let staged = unpack_verified(&archive, stage, staging.path()).unwrap();

        let replaced = replace_binaries(&bin, &staged, "0.12.1").unwrap();
        assert_eq!(replaced.binaries, vec!["phux"]);
        assert!(
            !bin.join("phux-mcp").exists(),
            "an install without phux-mcp must not acquire one"
        );
    }

    #[test]
    fn rollback_without_a_backup_is_an_error_not_a_no_op() {
        let scratch = Scratch::new("no-backup");
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        seed_bin_dir(&bin, 0o755);
        let err = rollback(&bin).unwrap_err();
        assert!(matches!(err, UpdateError::NoBackup(_)), "{err:?}");
        // The live binaries are untouched.
        assert_eq!(
            fs::read(bin.join("phux")).unwrap(),
            b"#!/bin/sh\nold phux\n"
        );
    }

    #[test]
    fn staging_is_a_sibling_of_the_target_and_is_cleaned_up() {
        let scratch = Scratch::new("staging");
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let path = {
            let staging = Staging::create(&bin).unwrap();
            assert_eq!(staging.path().parent(), Some(bin.as_path()));
            staging.path().to_path_buf()
        };
        assert!(!path.exists(), "the staging directory must be removed");
    }

    #[test]
    fn staging_refuses_an_unwritable_bin_directory() {
        let scratch = Scratch::new("readonly");
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o500)).unwrap();
        let result = Staging::create(&bin);
        // Restore the mode so the scratch guard can clean up.
        let _ = fs::set_permissions(&bin, fs::Permissions::from_mode(0o700));
        let Err(err) = result else {
            // Running as root (some CI images do): mode bits do not stop a
            // write, so there is no refusal to assert on.
            return;
        };
        assert!(
            format!("{err}").contains("must be writable"),
            "the refusal must say what is wrong: {err}"
        );
    }
}
