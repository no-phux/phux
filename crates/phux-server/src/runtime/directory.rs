//! `LIST_DIRECTORY` host query (`docs/spec/L3.md` §4).
//!
//! Lists the child directories of a path on the host this server runs on,
//! as the server's own OS user. The listing is a blocking filesystem walk,
//! so it runs on tokio's blocking pool under a deadline and replies from a
//! spawned task: a slow or hung filesystem delays only this one reply, never
//! the connection's frame loop or the single-threaded runtime.
//!
//! The walk is bounded twice: at most [`MAX_SCANNED_ENTRIES`] raw directory
//! entries are read, and at most [`MAX_DIRECTORY_ENTRIES`] child
//! directories are returned. Hitting either bound sets `truncated`.
//!
//! Security: this exposes nothing a connected client could not already learn
//! by spawning a shell as the same user (`docs/operations.md`, "Security
//! model and trust boundaries").

use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use phux_protocol::wire::frame::{
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError,
    DirectoryListingResult, FrameKind, MAX_DIRECTORY_ENTRIES,
};
use tokio::sync::Semaphore;
use tracing::{debug, trace};

use crate::state::{ClientId, Outbound, SharedState};

/// Most raw directory entries one listing reads before it stops and reports
/// truncation. Bounds the walk of a directory holding a huge number of files
/// even when few of them are directories.
const MAX_SCANNED_ENTRIES: usize = 16 * 1024;

/// How long the handler waits for the blocking walk before refusing with
/// [`DirectoryErrorCode::Other`].
const LIST_DEADLINE: Duration = Duration::from_secs(5);

/// Dispatch one `LIST_DIRECTORY`.
///
/// Replies only to an L3 consumer, matching the `GET_METADATA` gating
/// (`docs/spec/L3.md` §1.2). The reply is produced on a spawned task so the
/// caller's frame loop keeps running while the filesystem is walked.
pub(super) fn handle_list_directory(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    path: String,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let speaks_l3 = state.with(|s| s.client_speaks_l3(client_id));
    debug!(
        ?client_id,
        request_id,
        path = log_prefix(&path),
        path_bytes = path.len(),
        speaks_l3,
        "LIST_DIRECTORY"
    );
    if !speaks_l3 {
        return;
    }
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let result = list_request(path).await;
        let reply = FrameKind::DirectoryListing { request_id, result };
        if out_tx.send(Outbound::Frame(reply)).await.is_err() {
            trace!(
                ?client_id,
                request_id, "DIRECTORY_LISTING send dropped: writer gone"
            );
        }
    });
}

/// Longest request `path` accepted, in bytes (a `PATH_MAX`-sized bound).
/// A longer one is refused before it is cloned, logged in full, or handed
/// to the filesystem.
const MAX_REQUEST_PATH_BYTES: usize = 4096;

/// How much of a request path the debug log records.
const LOG_PATH_BYTES: usize = 256;

/// Most listings allowed to hold a blocking-pool thread at once.
///
/// The deadline stops the *reply* from waiting on a hung filesystem, but it
/// cannot stop the blocked thread: a walk stuck in the kernel keeps its
/// worker until the call returns. The pool is shared with uploads, log
/// rotation, and overlay-IP detection, so without a cap a client could leak
/// one thread per request against a hung mount. With it, at most this many
/// workers are ever stuck, and further requests are refused as busy.
const MAX_LISTINGS_IN_FLIGHT: usize = 8;

/// Permits for [`MAX_LISTINGS_IN_FLIGHT`], server-wide across connections.
static LISTINGS: Semaphore = Semaphore::const_new(MAX_LISTINGS_IN_FLIGHT);

/// Refuse an oversized path, then list under the server-wide cap.
async fn list_request(path: String) -> DirectoryListingResult {
    if path.len() > MAX_REQUEST_PATH_BYTES {
        return Err(other(
            log_prefix(&path),
            format!("path exceeds {MAX_REQUEST_PATH_BYTES} bytes"),
        ));
    }
    list_bounded(&LISTINGS, path).await
}

/// Run [`list_directory`] on the blocking pool, bounded by [`LIST_DEADLINE`]
/// and by the `slots` permits. The permit moves into the blocking closure,
/// so it is released only when the walk actually finishes, not when the
/// deadline gives up on it.
async fn list_bounded(slots: &'static Semaphore, path: String) -> DirectoryListingResult {
    let Ok(permit) = slots.try_acquire() else {
        return Err(other(&path, "too many directory listings in flight"));
    };
    let attempted = path.clone();
    let home = home_dir();
    let work = tokio::task::spawn_blocking(move || {
        let _held_until_the_walk_returns = permit;
        list_directory(
            &path,
            home.as_deref(),
            MAX_DIRECTORY_ENTRIES,
            MAX_SCANNED_ENTRIES,
        )
    });
    match tokio::time::timeout(LIST_DEADLINE, work).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_error)) => Err(other(
            &attempted,
            format!("listing worker failed: {join_error}"),
        )),
        Err(_elapsed) => Err(other(&attempted, "listing timed out")),
    }
}

/// At most [`LOG_PATH_BYTES`] of `path`, cut on a character boundary.
fn log_prefix(path: &str) -> &str {
    &path[..path.floor_char_boundary(LOG_PATH_BYTES)]
}

/// The serving user's home directory, from `$HOME`.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Resolve `request` and list its child directories. Blocking.
///
/// `request` is empty or `~` (the home directory), `~/rest`, or an absolute
/// path; anything else is refused. The resolved path is normalized
/// lexically (`.` dropped, `..` popped) without following symlinks, so the
/// reported path and parent are the ones the user navigated, not a
/// canonicalized spelling.
fn list_directory(
    request: &str,
    home: Option<&Path>,
    max_entries: usize,
    max_scanned: usize,
) -> DirectoryListingResult {
    let dir = resolve_request_path(request, home)?;
    let display = path_string(&dir);
    let scan = read_child_dirs(&dir, max_scanned).map_err(|e| io_refusal(&display, &e))?;
    Ok(finish_listing(display, &dir, scan, max_entries))
}

fn resolve_request_path(
    request: &str,
    home: Option<&Path>,
) -> Result<PathBuf, DirectoryListingError> {
    let raw = expand_home(request, home)?;
    if !raw.is_absolute() {
        return Err(other(
            request,
            "path must be absolute, `~`, `~/...`, or empty",
        ));
    }
    Ok(normalize_lexically(&raw))
}

/// Expand the home forms (`""`, `~`, `~/rest`); pass anything else through.
fn expand_home(request: &str, home: Option<&Path>) -> Result<PathBuf, DirectoryListingError> {
    let rest = match request {
        "" | "~" => Some(""),
        _ => request.strip_prefix("~/"),
    };
    let Some(rest) = rest else {
        return Ok(PathBuf::from(request));
    };
    let home =
        home.ok_or_else(|| other(request, "the serving user's home directory is unknown"))?;
    Ok(home.join(rest))
}

/// Drop `.` and resolve `..` by popping, without touching the filesystem.
/// `..` at the root stays at the root.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            kept => out.push(kept.as_os_str()),
        }
    }
    out
}

/// The child directories found by one bounded walk.
#[derive(Debug, Default)]
struct Scan {
    entries: Vec<DirectoryEntry>,
    truncated: bool,
}

/// Walk `dir`, keeping directories and symlinks that resolve to directories.
/// Reads at most `max_scanned` raw entries.
fn read_child_dirs(dir: &Path, max_scanned: usize) -> io::Result<Scan> {
    let mut scan = Scan::default();
    for (index, entry) in std::fs::read_dir(dir)?.enumerate() {
        if index >= max_scanned {
            scan.truncated = true;
            break;
        }
        if let Some(child) = entry.ok().as_ref().and_then(child_directory) {
            scan.entries.push(child);
        }
    }
    Ok(scan)
}

/// The listing row for `entry`, or `None` when it is not a directory, is a
/// dangling or non-directory symlink, or has a name that is not UTF-8.
fn child_directory(entry: &std::fs::DirEntry) -> Option<DirectoryEntry> {
    let name = entry.file_name().into_string().ok()?;
    let file_type = entry.file_type().ok()?;
    if file_type.is_dir() {
        return Some(DirectoryEntry {
            name,
            is_symlink: false,
        });
    }
    let symlinked_dir = file_type.is_symlink() && entry.path().is_dir();
    symlinked_dir.then_some(DirectoryEntry {
        name,
        is_symlink: true,
    })
}

/// Sort by name (byte order), apply the entry bound, and attach the parent.
fn finish_listing(
    path: String,
    dir: &Path,
    mut scan: Scan,
    max_entries: usize,
) -> DirectoryListing {
    scan.entries.sort_by(|a, b| a.name.cmp(&b.name));
    let truncated = scan.truncated || scan.entries.len() > max_entries;
    scan.entries.truncate(max_entries);
    DirectoryListing {
        path,
        parent: dir.parent().map(path_string),
        entries: scan.entries,
        truncated,
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn io_refusal(path: &str, error: &io::Error) -> DirectoryListingError {
    let code = match error.kind() {
        io::ErrorKind::NotFound => DirectoryErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => DirectoryErrorCode::PermissionDenied,
        io::ErrorKind::NotADirectory => DirectoryErrorCode::NotADirectory,
        _ => DirectoryErrorCode::Other,
    };
    DirectoryListingError {
        path: path.to_owned(),
        code,
        message: error.to_string(),
    }
}

fn other(path: &str, message: impl Into<String>) -> DirectoryListingError {
    DirectoryListingError {
        path: path.to_owned(),
        code: DirectoryErrorCode::Other,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    fn list(dir: &Path) -> DirectoryListingResult {
        list_directory(dir.to_str().unwrap(), None, 1024, 1024)
    }

    fn names(listing: &DirectoryListing) -> Vec<&str> {
        listing.entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn error_code(result: DirectoryListingResult) -> DirectoryErrorCode {
        result.expect_err("listing should be refused").code
    }

    #[test]
    fn lists_only_directories_sorted_by_byte_order() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["beta", "alpha", "Zeta", ".hidden"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }
        fs::write(tmp.path().join("file.txt"), b"x").unwrap();

        let listing = list(tmp.path()).unwrap();

        assert_eq!(names(&listing), [".hidden", "Zeta", "alpha", "beta"]);
        assert!(!listing.truncated);
        assert_eq!(listing.path, tmp.path().to_str().unwrap());
        assert_eq!(
            listing.parent.as_deref(),
            tmp.path().parent().and_then(Path::to_str)
        );
    }

    #[test]
    fn flags_directory_symlinks_and_skips_dangling_or_file_links() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("real")).unwrap();
        fs::write(tmp.path().join("file"), b"x").unwrap();
        symlink(tmp.path().join("real"), tmp.path().join("link")).unwrap();
        symlink(tmp.path().join("missing"), tmp.path().join("dead")).unwrap();
        symlink(tmp.path().join("file"), tmp.path().join("to-file")).unwrap();

        let listing = list(tmp.path()).unwrap();

        let flags: Vec<(&str, bool)> = listing
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.is_symlink))
            .collect();
        assert_eq!(flags, [("link", true), ("real", false)]);
    }

    #[test]
    fn truncates_to_the_entry_bound_keeping_the_first_names() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["e", "d", "c", "b", "a"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }

        let listing = list_directory(tmp.path().to_str().unwrap(), None, 3, 1024).unwrap();

        assert_eq!(names(&listing), ["a", "b", "c"]);
        assert!(listing.truncated);
    }

    #[test]
    fn truncates_when_the_scan_bound_is_hit() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["a", "b", "c", "d"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }

        let listing = list_directory(tmp.path().to_str().unwrap(), None, 1024, 2).unwrap();

        assert_eq!(listing.entries.len(), 2);
        assert!(listing.truncated);
    }

    #[test]
    fn missing_path_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let result = list(&tmp.path().join("nope"));
        assert_eq!(error_code(result), DirectoryErrorCode::NotFound);
    }

    #[test]
    fn a_file_is_not_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file.txt");
        fs::write(&file, b"x").unwrap();
        let refusal = list(&file).unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::NotADirectory);
        assert_eq!(refusal.path, file.to_str().unwrap());
    }

    #[test]
    fn unreadable_directory_is_permission_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // A privileged runner (root) can read a mode-000 directory anyway;
        // there is no denial to observe, so the case does not apply.
        let privileged = fs::read_dir(&locked).is_ok();
        let result = list(&locked);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        if privileged {
            return;
        }
        assert_eq!(error_code(result), DirectoryErrorCode::PermissionDenied);
    }

    #[test]
    fn home_forms_expand_against_the_given_home() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("proj")).unwrap();
        let home = Some(tmp.path());

        for request in ["", "~"] {
            let listing = list_directory(request, home, 1024, 1024).unwrap();
            assert_eq!(listing.path, tmp.path().to_str().unwrap());
            assert_eq!(names(&listing), ["proj"]);
        }
        let nested = list_directory("~/proj", home, 1024, 1024).unwrap();
        assert_eq!(nested.path, tmp.path().join("proj").to_str().unwrap());
    }

    #[test]
    fn home_forms_without_a_home_are_refused() {
        assert_eq!(
            error_code(list_directory("~", None, 1024, 1024)),
            DirectoryErrorCode::Other
        );
    }

    #[test]
    fn relative_paths_are_refused() {
        let refusal = list_directory("src/lib", None, 1024, 1024).unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        assert_eq!(refusal.path, "src/lib");
    }

    #[test]
    fn dot_segments_normalize_lexically() {
        assert_eq!(
            normalize_lexically(Path::new("/a/./b/../c/")),
            PathBuf::from("/a/c")
        );
        assert_eq!(normalize_lexically(Path::new("/../..")), PathBuf::from("/"));

        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        let via_dotdot = format!("{}/sub/..", tmp.path().to_str().unwrap());
        let listing = list_directory(&via_dotdot, None, 1024, 1024).unwrap();
        assert_eq!(listing.path, tmp.path().to_str().unwrap());
    }

    #[test]
    fn the_root_has_no_parent() {
        let listing = list_directory("/", None, 1024, 1024).unwrap();
        assert_eq!(listing.path, "/");
        assert_eq!(listing.parent, None);
    }

    #[tokio::test]
    async fn off_runtime_listing_returns_the_blocking_result() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("child")).unwrap();
        let listing = list_request(tmp.path().to_str().unwrap().to_owned())
            .await
            .unwrap();
        assert_eq!(names(&listing), ["child"]);
    }

    #[tokio::test]
    async fn a_full_listing_pool_is_refused_as_busy() {
        static EXHAUSTED: Semaphore = Semaphore::const_new(0);
        let refusal = list_bounded(&EXHAUSTED, "/".to_owned()).await.unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        assert_eq!(refusal.message, "too many directory listings in flight");
    }

    #[tokio::test]
    async fn a_permit_returns_to_the_pool_when_the_walk_finishes() {
        static ONE: Semaphore = Semaphore::const_new(1);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap().to_owned();
        list_bounded(&ONE, path.clone()).await.unwrap();
        list_bounded(&ONE, path).await.unwrap();
        assert_eq!(ONE.available_permits(), 1);
    }

    #[tokio::test]
    async fn an_overlong_path_is_refused_before_the_filesystem() {
        let path = format!("/{}", "a".repeat(MAX_REQUEST_PATH_BYTES));
        let refusal = list_request(path).await.unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        assert!(refusal.message.contains("exceeds 4096 bytes"));
        assert_eq!(refusal.path.len(), LOG_PATH_BYTES);
    }

    #[test]
    fn the_log_prefix_cuts_on_a_character_boundary() {
        let path = "é".repeat(LOG_PATH_BYTES);
        let prefix = log_prefix(&path);
        assert!(prefix.len() <= LOG_PATH_BYTES);
        assert!(prefix.chars().all(|c| c == 'é'));
        assert_eq!(log_prefix("/short"), "/short");
    }
}
