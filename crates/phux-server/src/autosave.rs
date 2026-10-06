//! Crash-safe workspace autosave (ADR-0150).
//!
//! `phux server --autosave PATH` keeps a workspace archive at `PATH` that a
//! crash, `SIGKILL`, `panic = "abort"`, or power loss cannot leave missing,
//! torn, or minutes stale. Two halves:
//!
//! - **Probe** (the server thread): once a second it hashes the restorable
//!   topology ([`crate::state::ServerState::workspace_revision`], in memory,
//!   no I/O) and publishes the value when it moved.
//! - **Saver** (its own OS thread): on a cold start it restores `PATH` once,
//!   then debounces revision changes ([`Debounce`]) and writes the archive
//!   atomically ([`write_atomic`]: temp, `fsync`, rename, directory `fsync`),
//!   with a periodic floor for drift the revision does not cover.
//!
//! What an archive *is* belongs to the embedder's [`WorkspaceArchiver`] (the
//! `phux` binary passes its `workspace save` / `restore` composition), so the
//! autosaved file and `phux workspace save` cannot drift. The saver dials the
//! server's own socket like any client; it never touches server state.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::state::{ServerState, SharedState};

/// How often the server thread re-hashes the workspace revision.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);

/// A change is saved once the revision has held still this long.
pub const QUIET: Duration = Duration::from_secs(2);

/// A workspace that never holds still is still saved this often.
pub const MAX_DELAY: Duration = Duration::from_secs(10);

/// Saved at least this often once armed, to catch drift the revision does
/// not hash (titles, pane sizes, the focused session).
pub const FLOOR: Duration = Duration::from_secs(60);

/// What an archive is: the embedder's capture and restore of the workspace
/// over the server's own socket.
pub trait WorkspaceArchiver: Send + 'static {
    /// Restore `archive` into the server listening on `socket`.
    ///
    /// # Errors
    ///
    /// Any failure, including a partial restore; the saver then keeps a copy
    /// of the archive before anything can overwrite it.
    fn restore(&mut self, socket: &Path, archive: &Path) -> Result<(), String>;

    /// Render the current workspace of the server listening on `socket`.
    ///
    /// # Errors
    ///
    /// Any capture failure; the save is retried after [`QUIET`].
    fn capture(&mut self, socket: &Path) -> Result<Vec<u8>, String>;
}

/// An autosave target: the archive path and how to fill it.
pub struct Autosave {
    path: PathBuf,
    archiver: Box<dyn WorkspaceArchiver>,
}

impl std::fmt::Debug for Autosave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Autosave")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Autosave {
    /// An autosave of `path` (made absolute, so a hot upgrade's re-exec names
    /// the same file).
    ///
    /// # Errors
    ///
    /// The path cannot be made absolute, or a development build names
    /// production state
    /// ([`phux_config::production::refuse_dev_on_production_state`]).
    pub fn new(path: &Path, archiver: Box<dyn WorkspaceArchiver>) -> Result<Self, String> {
        let path = std::path::absolute(path)
            .map_err(|err| format!("cannot resolve {}: {err}", path.display()))?;
        phux_config::production::refuse_dev_on_production_state(&path)?;
        Ok(Self { path, archiver })
    }

    /// The archive path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// When the saver writes. Pure, so the timing is unit-tested without
/// clocks or threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Debounce {
    /// The revision the archive last reflected; `None` until this process
    /// knows the archive matches anything live.
    saved: Option<u64>,
    /// The newest revision observed.
    latest: u64,
    /// When `latest` last moved while dirty.
    changed_at: Option<Instant>,
    /// When the archive first fell behind `latest`.
    dirty_since: Option<Instant>,
    /// The last save (or failed attempt), the floor's origin.
    last_save: Instant,
    /// Whether the floor saves an unchanged revision. Off after a failed
    /// restore until a change-driven save succeeds, so a server that could
    /// not load the archive does not overwrite it unprompted.
    floor_armed: bool,
}

impl Debounce {
    /// A policy whose archive already reflects `revision`: the floor stays
    /// off until a change is saved (the archive failed to restore and must
    /// not be overwritten unprompted).
    #[must_use]
    pub const fn saved_at(now: Instant, revision: u64) -> Self {
        Self {
            saved: Some(revision),
            latest: revision,
            changed_at: None,
            dirty_since: None,
            last_save: now,
            floor_armed: false,
        }
    }

    /// A policy that owes a save of `revision` after [`QUIET`]: the archive
    /// was restored (the live tree replaces it), absent, or this is a resume.
    #[must_use]
    pub const fn unsaved(now: Instant, revision: u64) -> Self {
        Self {
            saved: None,
            latest: revision,
            changed_at: Some(now),
            dirty_since: Some(now),
            last_save: now,
            floor_armed: true,
        }
    }

    fn dirty(&self) -> bool {
        self.saved != Some(self.latest)
    }

    /// The newest revision observed.
    #[must_use]
    pub const fn latest(&self) -> u64 {
        self.latest
    }

    /// Record the revision the probe published at `now`.
    pub fn observe(&mut self, now: Instant, revision: u64) {
        if revision == self.latest {
            return;
        }
        self.latest = revision;
        if !self.dirty() {
            // Changed back to what the archive holds: nothing to write.
            self.changed_at = None;
            self.dirty_since = None;
            return;
        }
        self.changed_at = Some(now);
        self.dirty_since.get_or_insert(now);
    }

    /// When the next save is due: the end of a quiet period (capped by
    /// [`MAX_DELAY`] from the first unsaved change), or the floor.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        let change = self
            .changed_at
            .zip(self.dirty_since)
            .map(|(changed, since)| (changed + QUIET).min(since + MAX_DELAY));
        let floor = self.floor_armed.then_some(self.last_save + FLOOR);
        match (change, floor) {
            (Some(change), Some(floor)) => Some(change.min(floor)),
            (change, floor) => change.or(floor),
        }
    }

    /// Whether a save is due at `now`.
    #[must_use]
    pub fn due(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| now >= deadline)
    }

    /// The archive now reflects `revision` (the latest seen when the capture
    /// began); a change that landed during the capture stays dirty.
    pub fn saved(&mut self, now: Instant, revision: u64) {
        self.saved = Some(revision);
        self.last_save = now;
        self.floor_armed = true;
        if self.latest == revision {
            self.changed_at = None;
            self.dirty_since = None;
        } else {
            self.dirty_since = Some(now);
            self.changed_at.get_or_insert(now);
        }
    }

    /// A save failed at `now`: retry a pending change after [`QUIET`], and
    /// push the floor out a full period.
    pub fn failed(&mut self, now: Instant) {
        self.last_save = now;
        if self.dirty() {
            self.changed_at = Some(now);
            self.dirty_since = Some(now);
        }
    }
}

/// The sibling temp file an autosave writes before the rename. Fixed (one
/// server per archive), so an interrupted write is overwritten, not leaked.
#[must_use]
pub fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".autosave.tmp");
    path.with_file_name(name)
}

/// Replace `path` with `bytes`, crash- and power-loss-safe.
///
/// Writes a `0600` sibling temp file, `fsync`s it, renames it over `path`,
/// then `fsync`s the directory so the rename itself is durable: any instant
/// leaves either the old file or the new one, never a torn one.
///
/// # Errors
///
/// Any I/O failure; the previous `path` is then untouched.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let tmp = temp_path(path);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(err) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    File::open(dir)?.sync_all()
}

/// Start the autosave: the probe on the current `LocalSet`, the saver on its
/// own thread. `cold_start` is false on a hot-upgrade resume, whose sessions
/// are already live and must not be restored again.
pub(crate) fn spawn(
    autosave: Autosave,
    state: &SharedState,
    root_token: &CancellationToken,
    socket_path: PathBuf,
    cold_start: bool,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let token = root_token.clone();
    let saver = Saver {
        autosave,
        socket_path,
        stop: root_token.clone(),
        last_written: None,
    };
    let path = saver.autosave.path.clone();
    let spawned = std::thread::Builder::new()
        .name("phux-autosave".to_owned())
        .spawn(move || saver.run(&rx, cold_start));
    if let Err(err) = spawned {
        warn!(path = %path.display(), error = %err, "autosave disabled: could not start its thread");
        return;
    }
    info!(path = %path.display(), cold_start, "workspace autosave armed");
    tokio::task::spawn_local(probe(state.clone(), tx, token));
}

/// Publish the workspace revision whenever it moves, until shutdown.
async fn probe(state: SharedState, tx: Sender<u64>, token: CancellationToken) {
    let mut tick = tokio::time::interval(PROBE_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = None;
    loop {
        tokio::select! {
            () = token.cancelled() => return,
            _ = tick.tick() => {}
        }
        let revision = state.with(ServerState::workspace_revision);
        if last != Some(revision) {
            if tx.send(revision).is_err() {
                return;
            }
            last = Some(revision);
        }
    }
}

/// The saver thread's state.
struct Saver {
    autosave: Autosave,
    socket_path: PathBuf,
    /// The server's root token: cancelled before any shutdown teardown.
    stop: CancellationToken,
    /// The bytes last written, so an unchanged capture is not rewritten.
    last_written: Option<Vec<u8>>,
}

impl Saver {
    fn run(mut self, rx: &Receiver<u64>, cold_start: bool) {
        let restore_failed = cold_start && !self.restore_once();
        if restore_failed {
            // Let the probe see whatever the failed restore left, so only a
            // later change overwrites the archive.
            std::thread::sleep(PROBE_INTERVAL + PROBE_INTERVAL / 2);
        }
        let Some(revision) = latest_revision(rx) else {
            return;
        };
        let now = Instant::now();
        let mut debounce = if restore_failed {
            Debounce::saved_at(now, revision)
        } else {
            Debounce::unsaved(now, revision)
        };
        loop {
            if self.stop.is_cancelled() {
                return;
            }
            let now = Instant::now();
            if debounce.due(now) {
                self.save(&mut debounce);
                continue;
            }
            let wait = debounce
                .deadline()
                .map_or(FLOOR, |deadline| deadline.saturating_duration_since(now));
            match rx.recv_timeout(wait) {
                Ok(revision) => debounce.observe(Instant::now(), revision),
                Err(RecvTimeoutError::Timeout) => {}
                // The probe ended: the server is shutting down.
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Restore the archive into the fresh server; `false` only when an
    /// archive exists and did not restore cleanly.
    fn restore_once(&mut self) -> bool {
        let path = self.autosave.path.clone();
        if !path.exists() {
            return true;
        }
        match self.autosave.archiver.restore(&self.socket_path, &path) {
            Ok(()) => {
                info!(path = %path.display(), "workspace autosave: restored the archive");
                true
            }
            Err(err) => {
                // Keep what could not be restored before autosave replaces it.
                let mut kept = path.clone().into_os_string();
                kept.push(".unrestored");
                let copied = fs::copy(&path, &kept).map(|_| ());
                warn!(
                    path = %path.display(),
                    error = %err,
                    kept = %Path::new(&kept).display(),
                    copied = copied.is_ok(),
                    "workspace autosave: restore failed; kept a copy of the archive",
                );
                false
            }
        }
    }

    fn save(&mut self, debounce: &mut Debounce) {
        let revision = debounce.latest();
        let path = self.autosave.path.clone();
        let bytes = match self.autosave.archiver.capture(&self.socket_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "workspace autosave: capture failed; retrying");
                debounce.failed(Instant::now());
                return;
            }
        };
        // Shutdown cancels the root token before it tears down a single pane,
        // so a capture that finished first is a whole workspace and one that
        // overlapped teardown is caught here (and ends the loop).
        if self.stop.is_cancelled() {
            return;
        }
        if self.last_written.as_deref() == Some(bytes.as_slice()) {
            debounce.saved(Instant::now(), revision);
            return;
        }
        match write_atomic(&path, &bytes) {
            Ok(()) => {
                self.last_written = Some(bytes);
                debounce.saved(Instant::now(), revision);
            }
            Err(err) => {
                warn!(path = %path.display(), error = %err, "workspace autosave: write failed; retrying");
                debounce.failed(Instant::now());
            }
        }
    }
}

/// The newest revision queued, waiting for one if none is; `None` once the
/// probe is gone.
fn latest_revision(rx: &Receiver<u64>) -> Option<u64> {
    let mut latest = rx.recv().ok()?;
    while let Ok(revision) = rx.try_recv() {
        latest = revision;
    }
    Some(latest)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    /// A policy that has saved `revision` at `now`, with the floor armed.
    fn armed(now: Instant, revision: u64) -> Debounce {
        let mut debounce = Debounce::unsaved(now, revision);
        debounce.saved(now, revision);
        debounce
    }

    #[test]
    fn a_fresh_or_restored_workspace_is_saved_after_the_quiet_period() {
        let t0 = Instant::now();
        let debounce = Debounce::unsaved(t0, 7);
        assert_eq!(debounce.deadline(), Some(t0 + QUIET));
        assert!(debounce.due(at(t0, 2)));
    }

    #[test]
    fn an_unchanged_workspace_waits_for_the_floor() {
        let t0 = Instant::now();
        let debounce = armed(t0, 7);
        assert_eq!(debounce.deadline(), Some(t0 + FLOOR));
        assert!(!debounce.due(at(t0, 59)));
        assert!(debounce.due(at(t0, 60)));
    }

    #[test]
    fn a_disarmed_floor_never_saves_an_unchanged_workspace() {
        let t0 = Instant::now();
        let debounce = Debounce::saved_at(t0, 7);
        assert_eq!(debounce.deadline(), None);
        assert!(!debounce.due(at(t0, 3600)));
    }

    #[test]
    fn a_change_saves_after_the_quiet_period() {
        let t0 = Instant::now();
        let mut debounce = armed(t0, 1);
        debounce.observe(at(t0, 5), 2);
        assert_eq!(debounce.deadline(), Some(at(t0, 5) + QUIET));
        // Another change restarts the quiet period.
        debounce.observe(at(t0, 6), 3);
        assert!(!debounce.due(at(t0, 7)));
        assert!(debounce.due(at(t0, 8)));
    }

    #[test]
    fn a_workspace_that_never_settles_still_saves_by_the_max_delay() {
        let t0 = Instant::now();
        let mut debounce = armed(t0, 0);
        for second in 1..=20 {
            debounce.observe(at(t0, second), second);
            if debounce.due(at(t0, second)) {
                assert_eq!(second, 1 + MAX_DELAY.as_secs());
                return;
            }
        }
        panic!("a constantly changing workspace was never saved");
    }

    #[test]
    fn changing_back_to_the_saved_revision_is_clean() {
        let t0 = Instant::now();
        let mut debounce = Debounce::saved_at(t0, 1);
        debounce.observe(at(t0, 1), 2);
        debounce.observe(at(t0, 2), 1);
        assert_eq!(debounce.deadline(), None);
    }

    #[test]
    fn a_change_during_the_capture_stays_dirty() {
        let t0 = Instant::now();
        let mut debounce = armed(t0, 1);
        debounce.observe(at(t0, 1), 2);
        let captured = debounce.latest();
        debounce.observe(at(t0, 4), 3);
        debounce.saved(at(t0, 4), captured);
        assert!(debounce.deadline().unwrap() <= at(t0, 4) + QUIET);
        debounce.saved(at(t0, 7), 3);
        assert_eq!(debounce.deadline(), Some(at(t0, 7) + FLOOR));
    }

    #[test]
    fn a_failed_save_retries_after_quiet_and_a_success_arms_the_floor() {
        let t0 = Instant::now();
        let mut debounce = Debounce::saved_at(t0, 1);
        debounce.observe(at(t0, 1), 2);
        debounce.failed(at(t0, 3));
        assert_eq!(debounce.deadline(), Some(at(t0, 3) + QUIET));
        debounce.saved(at(t0, 5), 2);
        assert_eq!(debounce.deadline(), Some(at(t0, 5) + FLOOR));
    }

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("workspace.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!temp_path(&path).exists());
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_failed_write_keeps_the_previous_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.json");
        write_atomic(&path, b"good").unwrap();
        // A directory squatting on the temp path makes the open fail.
        fs::create_dir(temp_path(&path)).unwrap();
        assert!(write_atomic(&path, b"torn").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"good");
    }
}
