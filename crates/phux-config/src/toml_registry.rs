//! Comment-preserving `[[remote]]` / `[[satellites]]` registry edits
//! (ADR-0038, ADR-0055) for the CLI and native embedders.
//!
//! [`edit_document`] holds a sibling advisory lock across read, modify, and
//! publish, refusing a busy writer immediately. Publication is atomic, follows
//! symlinks, and refuses when the file changed since it was read; that check
//! is not an atomic CAS against editors that do not take the lock.
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
    /// Refuses observed external changes and filesystem failures.
    pub fn commit(self) -> Result<(), String> {
        let text = self.document.to_string();
        crate::settings::replace_atomically(&self.path, &text, || {
            if read_text(&self.path).map_err(std::io::Error::other)? != self.original {
                return Err(std::io::Error::other(
                    "machine registry changed; refresh Machines",
                ));
            }
            #[cfg(test)]
            before_publish_hook();
            Ok(())
        })
        .map_err(|err| format!("could not write {}: {err}", self.path.display()))
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
/// Refuses busy writers, malformed documents and filesystem errors.
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
    // The machine registries point the production client at credentials.
    crate::production::refuse_dev_on_production_state(path)?;
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
/// Refuses changed, malformed, missing, ambiguous or inherited entries.
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

/// Validate a SHA-256 certificate pin: 64 hex digits, optionally with the
/// `AB:CD:...` separators `phux pair` prints. Catching a truncated paste at
/// registration beats a baffling handshake failure later.
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
    use super::{tables_mut, validate_fingerprint};

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

    /// An operator's comments survive, a symlinked config is written
    /// through (link and mode intact), and no temp file is left behind.
    #[cfg(unix)]
    #[test]
    fn commit_preserves_comments_symlinks_and_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("dotfiles").join("phux.toml");
        std::fs::create_dir_all(real.parent().unwrap()).expect("dotfiles dir");
        std::fs::write(&real, "# my notes\n[defaults]\nshell = \"fish\"\n").expect("seed");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).expect("mode");
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let mut edit = super::edit_document(&link).expect("parse");
        tables_mut(&mut edit, "remote")
            .expect("array")
            .push(toml_edit::Table::new());
        edit.commit().expect("write through symlink");

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let back = std::fs::read_to_string(&real).expect("read target");
        assert!(back.contains("# my notes") && back.contains("shell = \"fish\""));
        assert!(back.contains("[[remote]]"));
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let leftovers = std::fs::read_dir(real.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
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
