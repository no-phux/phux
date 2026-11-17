//! The one writer of a user `config.toml`: a locked, comment-preserving
//! read/modify/publish transaction.
//!
//! Settings edits, the machine registries (`[[remote]]`, `[[satellites]]`),
//! the plugin registry, and the distro slot all edit the same file, so they
//! all go through [`ConfigDocument`]. [`ConfigDocument::open`] resolves one
//! target, takes its sibling advisory lock (`<name>.registry.lock`, waiting
//! up to [`LOCK_WAIT`] for another holder), then reads and parses the file.
//! [`ConfigDocument::commit`] publishes through an fsynced sibling temp file
//! and a rename, still holding the lock, and refuses when the file changed
//! since it was read. That check is not an atomic compare-and-swap against
//! editors that do not take the lock; between phux writers it is.
//!
//! Two decisions vary by caller:
//!
//! - [`Guard`]: whether a development build may write this path at all.
//!   The registries, plugins and distro slot decide what the production
//!   client connects to and what code it runs, so they refuse a dev build
//!   aimed at production state ([`Guard::ProductionState`]). Plain settings
//!   are exempt ([`Guard::SharedSettings`]): `config_path` is not split by
//!   profile, so a dev build still edits settings, theme and omarchy.
//! - [`SymlinkPolicy`]: settings and registry edits follow an existing link
//!   (the link survives, its target is replaced). Plugin commands refuse
//!   any symlinked config, because that file decides what code runs. A
//!   dangling link is refused in both modes rather than replaced.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

/// Whether a development build may write the document's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Refuse a development build aimed at production state
    /// ([`crate::production::refuse_dev_on_production_state`]).
    ProductionState,
    /// Plain settings shared by every profile: no production guard.
    SharedSettings,
}

/// What to do when the path is a symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkPolicy {
    /// Edit the link's existing target; the link survives.
    FollowExisting,
    /// Refuse any symlink at the path.
    Refuse,
}

/// Why a document could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    /// Refused or failed before the contents were parsed: the production
    /// guard, the symlink policy, a busy lock, or a filesystem error.
    #[error("{0}")]
    Unavailable(String),
    /// The file is not valid TOML.
    #[error("could not parse {}: {}", .0.path.display(), .0.error)]
    Parse(Box<ParseFailure>),
}

/// A config file that failed to parse, with the text for positioned errors.
#[derive(Debug)]
pub struct ParseFailure {
    /// The resolved file.
    pub path: PathBuf,
    /// The text that failed to parse.
    pub text: String,
    /// The parser's error.
    pub error: toml_edit::TomlError,
}

impl From<DocumentError> for String {
    fn from(err: DocumentError) -> Self {
        err.to_string()
    }
}

/// One locked read/modify/publish transaction on a config file. Derefs to
/// the parsed [`DocumentMut`]; dropping it without [`Self::commit`] releases
/// the lock and writes nothing.
pub struct ConfigDocument {
    path: PathBuf,
    document: DocumentMut,
    original: String,
    stale_message: &'static str,
    _lock: std::fs::File,
}

impl std::fmt::Debug for ConfigDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigDocument")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for ConfigDocument {
    type Target = DocumentMut;
    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

impl std::ops::DerefMut for ConfigDocument {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.document
    }
}

impl ConfigDocument {
    /// Begin a transaction, acquiring the lock before reading contents. A
    /// missing file opens as an empty document (its parent is created).
    ///
    /// # Errors
    /// [`DocumentError::Unavailable`] for a guard or symlink refusal, a busy
    /// lock, or a filesystem error; [`DocumentError::Parse`] for bad TOML.
    pub fn open(path: &Path, guard: Guard, symlink: SymlinkPolicy) -> Result<Self, DocumentError> {
        check_guard(guard, path)?;
        if symlink == SymlinkPolicy::Refuse {
            refuse_symlink(path)?;
        }
        let path = resolve_target(path)?;
        check_guard(guard, &path)?;
        let lock = lock_file(&path)?;
        let original = read_text(&path)?;
        let document = match original.parse::<DocumentMut>() {
            Ok(document) => document,
            Err(error) => {
                return Err(DocumentError::Parse(Box::new(ParseFailure {
                    path,
                    text: original,
                    error,
                })));
            }
        };
        Ok(Self {
            path,
            document,
            original,
            stale_message: STALE_MESSAGE,
            _lock: lock,
        })
    }

    /// Replace the error [`Self::commit`] reports when the file changed on
    /// disk since it was read.
    #[must_use]
    pub const fn with_stale_message(mut self, message: &'static str) -> Self {
        self.stale_message = message;
        self
    }

    /// The file's text as it was read under the lock.
    #[must_use]
    pub fn original(&self) -> &str {
        &self.original
    }

    /// Whether the document now differs from what was read.
    #[must_use]
    pub fn is_changed(&self) -> bool {
        self.document.to_string() != self.original
    }

    /// Publish this edit while retaining the lock through the rename.
    ///
    /// # Errors
    /// Refuses observed external changes and filesystem failures.
    pub fn commit(self) -> Result<(), String> {
        let path = self.path.clone();
        self.publish()
            .map_err(|err| format!("could not write {}: {err}", path.display()))
    }

    /// [`Self::commit`] with the underlying I/O error, for callers that
    /// carry their own path in the error.
    pub(crate) fn publish(self) -> std::io::Result<()> {
        let text = self.document.to_string();
        replace_atomically(&self.path, &text, || {
            if read_text(&self.path).map_err(std::io::Error::other)? != self.original {
                return Err(std::io::Error::other(self.stale_message));
            }
            #[cfg(test)]
            before_publish_hook();
            Ok(())
        })
    }
}

/// The default [`ConfigDocument::commit`] refusal for a stale read.
const STALE_MESSAGE: &str = "config file changed on disk since it was read; retry";

/// How long [`ConfigDocument::open`] waits for another holder of the lock.
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

fn check_guard(guard: Guard, path: &Path) -> Result<(), DocumentError> {
    match guard {
        Guard::ProductionState => crate::production::refuse_dev_on_production_state(path)
            .map_err(DocumentError::Unavailable),
        Guard::SharedSettings => Ok(()),
    }
}

fn refuse_symlink(path: &Path) -> Result<(), DocumentError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DocumentError::Unavailable(
            format!("{} must not be a symlink", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(DocumentError::Unavailable(format!(
            "could not inspect {}: {err}",
            path.display()
        ))),
    }
}

/// Existing files use their real target; new files use a real parent plus
/// their filename. Never treat a dangling file symlink as an independent file.
fn resolve_target(path: &Path) -> Result<PathBuf, DocumentError> {
    let unavailable = |message: String| DocumentError::Unavailable(message);
    let name = path
        .file_name()
        .ok_or_else(|| unavailable(format!("{} has no file name", path.display())))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|err| unavailable(err.to_string()))?;
    let resolved = match std::fs::symlink_metadata(path) {
        Ok(_) => std::fs::canonicalize(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::canonicalize(parent).map(|parent| parent.join(name))
        }
        Err(err) => Err(err),
    };
    resolved.map_err(|err| unavailable(format!("could not resolve {}: {err}", path.display())))
}

fn lock_file(path: &Path) -> Result<std::fs::File, DocumentError> {
    let unavailable = |message: String| DocumentError::Unavailable(message);
    let parent = path
        .parent()
        .ok_or_else(|| unavailable(format!("{} has no parent directory", path.display())))?;
    let mut name = path
        .file_name()
        .ok_or_else(|| unavailable(format!("{} has no file name", path.display())))?
        .to_os_string();
    name.push(".registry.lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(name))
        .map_err(|err| unavailable(err.to_string()))?;
    // Another phux editing the file holds it only for a read and a write,
    // and a child process being spawned holds an inherited lock only until
    // it execs: wait that out rather than fail on a brief overlap.
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(err) => {
                return Err(unavailable(format!(
                    "config is busy or could not be locked; retry: {err}"
                )));
            }
        }
    }
}

fn read_text(path: &Path) -> Result<String, DocumentError> {
    match std::fs::read_to_string(path) {
        Ok(input) => Ok(input),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(DocumentError::Unavailable(format!(
            "could not read {}: {err}",
            path.display()
        ))),
    }
}

/// Replace `path` with `text` via an exclusive, fsynced sibling temp file
/// and a rename, so an interrupted write never truncates the config.
///
/// A symlink is followed (the link survives) and the target's permissions
/// are copied. `before_rename` runs after the fsync and may veto the
/// publication; the temp file is removed on any failure.
fn replace_atomically(
    path: &Path,
    text: &str,
    before_rename: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| Path::new("."), Path::new);
    std::fs::create_dir_all(parent)?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
        })?;
    let tmp = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        temp_nonce()
    ));
    let publish = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if let Ok(metadata) = std::fs::metadata(&target) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        before_rename()?;
        std::fs::rename(&tmp, &target)
    };
    publish().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A temp-path suffix so concurrent writers do not collide.
fn temp_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
thread_local! {
    // Deterministic scheduler boundary, not an alternative writer implementation.
    pub(crate) static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn before_publish_hook() {
    let hook = BEFORE_PUBLISH.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}
