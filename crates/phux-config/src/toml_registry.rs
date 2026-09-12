//! Shared `config.toml` array-of-tables plumbing for the CLI's registries.
//!
//! The satellite registry (ADR-0038) and the remote registry (ADR-0055),
//! both reachable through `phux host --role satellite|remote` (ADR-0066),
//! each maintain an array of tables in the user's `config.toml`, and both
//! must do it without destroying the operator's comments and formatting —
//! hence `toml_edit` rather than a serialize round-trip.
//!
//! The document-level discipline is identical for both and lives here once:
//! refuse a symlinked config (a registry write must not be redirected into a
//! file the operator did not mean to edit), and replace the config
//! atomically via a temp file plus rename, so an interrupted write cannot
//! leave a truncated config that fails to parse on the next start.
//!
//! `edit_document` holds a sibling advisory lock throughout read/modify/publish.
//! Shared CLI remote/satellite writers and native Forget cooperate with it. A busy
//! writer is refused immediately so a GUI request cannot wait behind a CLI.
//! The empty sibling lock file persists; closing the handle releases the OS lock.
//! Keeping its inode stable prevents two cooperating writers locking different
//! files during unlink/recreate races.
//! Arbitrary editors do not necessarily take this lock: byte comparisons detect
//! their observed changes, but comparison plus rename is not an atomic filesystem
//! CAS against noncooperating editors. No stronger guarantee is claimed.
//!
//! What stays with each registry is its schema: field names, validation, and
//! the meaning of an entry.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

/// One locked registry read/modify/publish transaction.
pub struct RegistryEdit {
    path: PathBuf,
    document: DocumentMut,
    original: String,
    _lock: std::fs::File,
}

impl std::fmt::Debug for RegistryEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryEdit")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for RegistryEdit {
    type Target = DocumentMut;
    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

impl std::ops::DerefMut for RegistryEdit {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.document
    }
}

impl RegistryEdit {
    /// Publish this edit while retaining its lock through the rename.
    /// # Errors
    /// Refuses observed external changes, symlinks and filesystem failures.
    pub fn commit(self) -> Result<(), String> {
        publish_document(&self.path, &self.document, Some(&self.original))
    }

    /// Remove one exact root machine, never an inherited or ambiguous entry.
    /// # Errors
    /// Refuses unknown roles, missing entries and duplicate name/endpoint pairs.
    pub fn remove_machine(&mut self, role: &str, name: &str, endpoint: &str) -> Result<(), String> {
        if !matches!(role, "remote" | "satellites") {
            return Err("unknown machine registry role".to_owned());
        }
        let tables = tables_mut(&mut self.document, role)?;
        let matches: Vec<_> = tables
            .iter()
            .enumerate()
            .filter_map(|(index, table)| {
                (table.get("name").and_then(Item::as_str) == Some(name)
                    && table.get("endpoint").and_then(Item::as_str) == Some(endpoint))
                .then_some(index)
            })
            .collect();
        let [index] = matches.as_slice() else {
            return Err(
                "machine is ambiguous or inherited; edit its source configuration".to_owned(),
            );
        };
        tables.remove(*index);
        Ok(())
    }
}

/// Begin a shared registry mutation, acquiring its lock before reading anything.
/// # Errors
/// Refuses busy writers, symlinks, malformed documents and filesystem errors.
pub fn edit_document(path: &Path) -> Result<RegistryEdit, String> {
    let lock = lock_registry(path)?;
    let original = read_text(path)?;
    let document = original
        .parse::<DocumentMut>()
        .map_err(|err| format!("could not parse {}: {err}", path.display()))?;
    Ok(RegistryEdit {
        path: path.to_path_buf(),
        document,
        original,
        _lock: lock,
    })
}

fn lock_registry(path: &Path) -> Result<std::fs::File, String> {
    reject_symlink(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| "registry has no parent directory".to_owned())?;
    std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    let mut name = path
        .file_name()
        .ok_or_else(|| "registry has no file name".to_owned())?
        .to_os_string();
    name.push(".registry.lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(name))
        .map_err(|err| err.to_string())?;
    lock.try_lock()
        .map_err(|err| format!("registry is busy or could not be locked; retry: {err}"))?;
    Ok(lock)
}

fn read_text(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(input) => Ok(input),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("could not read {}: {err}", path.display())),
    }
}

/// Remove one captured machine entry in the root user file, preserving comments.
///
/// `expected` is the exact root document captured when the entry was displayed.
/// Inherited entries are not writable through this seam. Roles use the config's
/// keys (`remote` or `satellites`), never a combined inventory row offset.
///
/// # Errors
/// Refuses changed, malformed, symlinked, missing, ambiguous or inherited entries.
pub fn forget_machine(
    path: &Path,
    expected: &str,
    role: &str,
    name: &str,
    endpoint: &str,
) -> Result<(), String> {
    let mut edit = edit_document(path)?;
    if edit.original != expected {
        return Err("machine registry changed; refresh Machines".to_owned());
    }
    edit.remove_machine(role, name, endpoint)?;
    edit.commit()
}

/// Parse `config.toml`, treating a missing file as an empty document — a
/// first `add` on a machine with no config must succeed.
pub fn read_document(config_path: &Path) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(config_path) {
        Ok(input) => input
            .parse::<DocumentMut>()
            .map_err(|err| format!("could not parse {}: {err}", config_path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(err) => Err(format!("could not read {}: {err}", config_path.display())),
    }
}

/// Replace `config.toml` atomically: write a sibling temp file, fsync, then
/// rename over the target. A crash mid-write leaves the old config intact.
///
/// This is an unconditional replacement. Read/modify callers use [`edit_document`]
/// to retain the lock from their read through publication.
pub fn write_document(config_path: &Path, doc: &DocumentMut) -> Result<(), String> {
    let _lock = lock_registry(config_path)?;
    publish_document(config_path, doc, None)
}

fn publish_document(
    config_path: &Path,
    doc: &DocumentMut,
    expected: Option<&str>,
) -> Result<(), String> {
    reject_symlink(config_path)?;
    let parent = config_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", config_path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", config_path.display()))?;
    let tmp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        temp_nonce()
    ));
    let write_result = write_temp_file(&tmp_path, doc.to_string().as_bytes())
        .and_then(|()| verify_expected(config_path, expected))
        .and_then(|()| {
            #[cfg(test)]
            before_publish_hook();
            std::fs::rename(&tmp_path, config_path).map_err(|err| err.to_string())
        });
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not write {}: {err}", config_path.display()));
    }
    Ok(())
}

// Check after the temp file is fsynced, immediately before publication; unrelated
// external edits observed during parsing/writing preserve the source file.
fn verify_expected(path: &Path, expected: Option<&str>) -> Result<(), String> {
    let Some(expected) = expected else {
        return Ok(());
    };
    reject_symlink(path)?;
    if read_text(path)? != expected {
        return Err("machine registry changed; refresh Machines".to_owned());
    }
    Ok(())
}

/// The array of tables under `key`, creating it when absent.
pub fn tables_mut<'doc>(
    doc: &'doc mut DocumentMut,
    key: &str,
) -> Result<&'doc mut ArrayOfTables, String> {
    let item = doc
        .as_table_mut()
        .entry(key)
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
    item.as_array_of_tables_mut()
        .ok_or_else(|| format!("`{key}` must be an array of tables"))
}

/// One entry of the array of tables under `key`.
pub fn table_mut<'doc>(
    doc: &'doc mut DocumentMut,
    key: &str,
    index: usize,
) -> Result<&'doc mut Table, String> {
    tables_mut(doc, key)?
        .get_mut(index)
        .ok_or_else(|| format!("`{key}` registry index {index} disappeared"))
}

/// Refuse to write through a symlink: a registry write must land in the file
/// the operator's config path names, not wherever a link points.
pub fn reject_symlink(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("{} must not be a symlink", path.display()))
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("could not inspect {}: {err}", path.display())),
    }
}

/// A SHA-256 certificate pin: exactly 64 hex digits once the `AB:CD:...`
/// separators `phux pair` prints are dropped.
///
/// Validating at registration turns a truncated copy-paste into an error
/// where the operator can still see what they pasted, instead of a baffling
/// handshake failure later. Shared because both registries pin the same way
/// (ADR-0038's fail-closed posture).
pub fn validate_fingerprint(fingerprint: &str, what: &str) -> Result<String, String> {
    let trimmed = fingerprint.trim();
    let hex_digits = trimmed.chars().filter(char::is_ascii_hexdigit).count();
    let separators_only = trimmed
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == ':' || c.is_whitespace());
    if hex_digits == 64 && separators_only {
        Ok(trimmed.to_owned())
    } else {
        Err(format!(
            "{what} cert-fingerprint must be a SHA-256 fingerprint (64 hex digits, \
             optionally colon-separated) as printed by `phux pair`"
        ))
    }
}

/// A monotonic-enough suffix to keep two concurrent writers from colliding on
/// the same temp path.
fn temp_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

/// Create the temp file exclusively and fsync it, so the rename that follows
/// publishes bytes that are actually on disk.
fn write_temp_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    file.write_all(bytes).map_err(|err| err.to_string())?;
    file.sync_all().map_err(|err| err.to_string())
}

#[cfg(test)]
thread_local! {
    // Deterministic scheduler boundary, not an alternative writer implementation.
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn before_publish_hook() {
    let hook = BEFORE_PUBLISH.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
mod tests {
    use super::{read_document, tables_mut, validate_fingerprint, write_document};

    #[test]
    fn review_competing_registry_mutation_cannot_be_lost_during_publication() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let raw =
            "[[remote]]\nname='a'\nendpoint='ssh://a'\n[[remote]]\nname='b'\nendpoint='ssh://b'\n";
        std::fs::write(&path, raw).expect("seed");
        let nested_succeeded = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = nested_succeeded.clone();
        let competing_path = path.clone();
        super::BEFORE_PUBLISH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                observed.set(
                    super::forget_machine(&competing_path, raw, "remote", "b", "ssh://b").is_ok(),
                );
            }));
        });
        super::forget_machine(&path, raw, "remote", "a", "ssh://a").expect("outer forget");
        assert!(
            !nested_succeeded.get(),
            "a cooperating writer committed inside another writer's checked publication"
        );
        let after = std::fs::read_to_string(path).expect("read");
        assert!(after.contains("name='b'"));
        assert!(!after.contains("name='a'"));
    }

    #[test]
    fn transaction_lock_covers_read_modify_publish_and_releases_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut first = super::edit_document(&path).expect("first transaction");
        assert!(
            super::edit_document(&path).is_err(),
            "second writer cannot read under first writer's lock"
        );
        first["first"] = toml_edit::value(1);
        first.commit().expect("publish first");
        let mut second = super::edit_document(&path).expect("lock released");
        assert_eq!(second["first"].as_integer(), Some(1));
        second["second"] = toml_edit::value(2);
        second.commit().expect("publish second");
        let third = super::edit_document(&path).expect("third transaction");
        std::fs::write(&path, "# external edit\n").expect("noncooperating editor");
        assert!(
            third.commit().is_err(),
            "observed external changes are refused"
        );
        assert_eq!(
            std::fs::read_to_string(path).expect("preserved"),
            "# external edit\n"
        );
    }

    #[test]
    fn round_trip_preserves_operator_comments() {
        // The reason this uses toml_edit: an operator's config is theirs.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# my notes\n[defaults]\nshell = \"fish\"\n").expect("seed");

        let mut doc = read_document(&path).expect("parse");
        tables_mut(&mut doc, "remote")
            .expect("array")
            .push(toml_edit::Table::new());
        write_document(&path, &doc).expect("write");

        let back = std::fs::read_to_string(&path).expect("read");
        assert!(back.contains("# my notes"), "comment must survive");
        assert!(back.contains("shell = \"fish\""));
        assert!(back.contains("[[remote]]"));
    }

    #[test]
    fn missing_config_reads_as_an_empty_document() {
        let dir = tempfile::tempdir().expect("tempdir");
        let doc = read_document(&dir.path().join("absent.toml")).expect("empty");
        assert!(doc.as_table().is_empty());
    }

    #[test]
    fn write_refuses_a_symlinked_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real.toml");
        std::fs::write(&real, "").expect("seed");
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let err = write_document(&link, &read_document(&real).expect("parse"))
            .expect_err("symlink must be refused");
        assert!(err.contains("must not be a symlink"), "got {err}");
    }

    #[test]
    fn write_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        write_document(&path, &read_document(&path).expect("parse")).expect("write");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .filter(|name| name.to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
    }

    #[test]
    fn fingerprint_accepts_both_shapes_pair_prints() {
        let bare = "ab".repeat(32);
        assert_eq!(validate_fingerprint(&bare, "remote").as_deref(), Ok(&*bare));
        let colon = (0..32).map(|_| "AB").collect::<Vec<_>>().join(":");
        assert!(validate_fingerprint(&colon, "remote").is_ok());
        // Surrounding whitespace from a paste is trimmed, not rejected.
        assert!(validate_fingerprint(&format!("  {bare}\n"), "remote").is_ok());
    }

    #[test]
    fn fingerprint_rejects_a_truncated_paste() {
        // 63 digits — the classic one-character-short copy.
        let short = "a".repeat(63);
        assert!(validate_fingerprint(&short, "remote").is_err());
        assert!(validate_fingerprint("", "remote").is_err());
        assert!(validate_fingerprint(&"z".repeat(64), "remote").is_err());
        // The message must name which registry rejected it.
        let err = validate_fingerprint("nope", "satellite").expect_err("reject");
        assert!(err.starts_with("satellite cert-fingerprint"), "got {err}");
    }
}
