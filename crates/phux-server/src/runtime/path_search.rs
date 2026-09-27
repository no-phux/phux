//! Bounded, host-local path discovery for the path picker.
//!
//! The index is an ephemeral cache of a *specified* root, not a privileged
//! filesystem crawler. A cold query walks on a blocking worker (the caller
//! owns that scheduling and its outer timeout); warm queries only rank cached
//! paths. Results name the serving host's paths, never the consumer's disk.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{
    FrameKind, PathErrorCode, PathKind, PathQueryError, PathQueryResult, PathResults, PathRow,
    PathStatus,
};
use tokio::sync::Semaphore;

use crate::hub::relay::RelayHandle;
use crate::state::{ClientId, Outbound, SharedState};

use super::directory;

const MAX_DEPTH: usize = 12;
const MAX_VISITED: usize = 16_384;
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHED_ROOTS: usize = 4;
const MAX_RESULTS: usize = 100;
const WALK_BUDGET: Duration = Duration::from_millis(250);
const CACHE_AGE: Duration = Duration::from_secs(10);
const MAX_ROOT_BYTES: usize = 4096;
const MAX_QUERY_BYTES: usize = 256;
const QUERY_DEADLINE: Duration = Duration::from_secs(5);
static PATH_WORKERS: Semaphore = Semaphore::const_new(8);
static INDEX: OnceLock<PathIndex> = OnceLock::new();

/// The wire query, including the intended satellite (when routed by a hub).
pub(super) struct PathRequest {
    pub(super) request_id: u32,
    pub(super) root: String,
    pub(super) query: String,
    pub(super) recursive: bool,
    pub(super) host: Option<SatelliteHost>,
}

enum PathRoute {
    Local,
    Relay(RelayHandle),
    Refused(String),
}

fn path_route(state: &SharedState, host: Option<SatelliteHost>) -> PathRoute {
    let Some(host) = host else {
        return PathRoute::Local;
    };
    state.with(|server| match server.hub_relay(&host) {
        Some(relay) => PathRoute::Relay(relay),
        None if server.hub_table().is_some() => {
            PathRoute::Refused(format!("no satellite named {host} in this hub's registry"))
        }
        None => PathRoute::Refused(format!(
            "this server is not a federation hub; it has no route to satellite {host}"
        )),
    })
}

/// Dispatch without holding up the connection's frame loop. Closing the
/// outbound mailbox cancels the wait, while a blocked worker retains its
/// permit until it actually returns from the filesystem.
pub(super) fn handle_path_query(
    state: &SharedState,
    client_id: ClientId,
    request: PathRequest,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    if !state.with(|s| s.client_speaks_l3(client_id)) {
        return;
    }
    let route = path_route(state, request.host);
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            result = answer(route, request.root, request.query, request.recursive) => result,
            () = out_tx.closed() => return,
        };
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::PathResults {
                request_id: request.request_id,
                result,
            }))
            .await;
    });
}

async fn answer(route: PathRoute, root: String, query: String, recursive: bool) -> PathQueryResult {
    if root.len() > MAX_ROOT_BYTES
        || root.contains('\0')
        || query.len() > MAX_QUERY_BYTES
        || query.contains('\0')
    {
        return Err(refusal(&root, "invalid path query length or NUL byte"));
    }
    if recursive && query.is_empty() {
        return Err(refusal(&root, "recursive search requires a query"));
    }
    match route {
        PathRoute::Local => local_request(root, query, recursive).await,
        PathRoute::Relay(relay) => relay_request(relay, root, query, recursive).await,
        PathRoute::Refused(message) => Err(refusal(&root, message)),
    }
}

async fn relay_request(
    relay: RelayHandle,
    root: String,
    query: String,
    recursive: bool,
) -> PathQueryResult {
    let Some(_host_slot) = directory::RELAYED_PER_HOST.try_acquire(relay.host()) else {
        return Err(refusal(
            &root,
            format!(
                "satellite {} already has too many host queries in flight",
                relay.host()
            ),
        ));
    };
    let Ok(_permit) = directory::RELAYED_LISTINGS.try_acquire() else {
        return Err(refusal(&root, "too many relayed host queries in flight"));
    };
    relay.path_query(root, query, recursive).await
}

async fn local_request(root: String, query: String, recursive: bool) -> PathQueryResult {
    let Ok(permit) = PATH_WORKERS.try_acquire() else {
        return Err(refusal(&root, "too many path searches in flight; retry"));
    };
    let attempted = root.clone();
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        query_local(
            &root,
            &query,
            recursive,
            directory::home_dir().as_deref(),
            INDEX.get_or_init(PathIndex::default),
        )
    });
    match tokio::time::timeout(QUERY_DEADLINE, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(refusal(
            &attempted,
            format!("path query worker failed: {error}"),
        )),
        Err(_) => Err(refusal(&attempted, "path query timed out")),
    }
}

fn query_local(
    requested: &str,
    query: &str,
    recursive: bool,
    home: Option<&Path>,
    index: &PathIndex,
) -> PathQueryResult {
    let root = directory::resolve_request_path(requested, home)
        .map_err(|error| refusal(requested, error.message))?;
    let Some(display) = root.to_str() else {
        return Err(refusal(
            requested,
            "the serving user's home path is not UTF-8",
        ));
    };
    if display.len() > MAX_ROOT_BYTES {
        return Err(refusal(requested, "resolved root exceeds 4096 bytes"));
    }
    let result = index
        .query(&root, query, recursive)
        .map_err(|error| PathQueryError {
            root: display.to_owned(),
            code: match error.kind() {
                io::ErrorKind::NotFound => PathErrorCode::NotFound,
                io::ErrorKind::PermissionDenied => PathErrorCode::PermissionDenied,
                io::ErrorKind::NotADirectory => PathErrorCode::NotADirectory,
                _ => PathErrorCode::Other,
            },
            message: error.to_string(),
        })?;
    tracing::trace!(
        from_cache = result.from_cache,
        rows = result.hits.len(),
        "path query answered"
    );
    let rows = result
        .hits
        .into_iter()
        .filter_map(|hit| {
            let path = hit.path.into_os_string().into_string().ok()?;
            if path.contains('\0') {
                return None;
            }
            let kind = if hit.is_symlink {
                PathKind::Symlink
            } else if hit.is_dir {
                PathKind::Directory
            } else {
                PathKind::File
            };
            Some(PathRow { path, kind })
        })
        .collect();
    Ok(PathResults {
        root: display.to_owned(),
        parent: root.parent().and_then(Path::to_str).map(str::to_owned),
        rows,
        status: if result.truncated {
            PathStatus::Truncated
        } else {
            PathStatus::Complete
        },
    })
}

fn refusal(root: &str, message: impl Into<String>) -> PathQueryError {
    PathQueryError {
        root: root.to_owned(),
        code: PathErrorCode::Other,
        message: message.into(),
    }
}

/// A path and its kind, retained as a `PathBuf` so Unix filenames that are
/// not UTF-8 are never silently changed before presentation or insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PathHit {
    pub(super) path: PathBuf,
    pub(super) is_dir: bool,
    pub(super) is_symlink: bool,
}

/// The result is explicitly incomplete when a bound stopped the walk.
#[derive(Debug)]
pub(super) struct SearchResult {
    pub(super) hits: Vec<PathHit>,
    pub(super) truncated: bool,
    pub(super) from_cache: bool,
}

#[derive(Clone)]
struct Snapshot {
    entries: Vec<PathHit>,
    scanned_at: Instant,
    truncated: bool,
    root_identity: (u64, u64),
    root_modified: Option<std::time::SystemTime>,
}

/// A small, per-process cache. A 10-second expiry is intentional: filesystem
/// changes become visible without maintaining a platform-specific watcher.
#[derive(Default)]
pub(super) struct PathIndex {
    snapshots: Mutex<HashMap<PathBuf, Snapshot>>,
}

impl PathIndex {
    /// Browse immediate children or fuzzy-search descendants of `root`.
    /// Call from a bounded blocking worker, never the server's event loop.
    pub(super) fn query(
        &self,
        root: &Path,
        needle: &str,
        recursive: bool,
    ) -> io::Result<SearchResult> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path-search root must be absolute",
            ));
        }
        let root = root.to_path_buf();
        let metadata = std::fs::metadata(&root)?;
        if !metadata.is_dir() {
            return Err(io::Error::from(io::ErrorKind::NotADirectory));
        }
        // Do not return a cached snapshot after the root loses read access.
        std::fs::read_dir(&root)?;
        let cached = recursive.then(|| self.cached(&root, &metadata)).flatten();
        let from_cache = cached.is_some();
        let snapshot = match cached {
            Some(snapshot) => snapshot,
            None => scan(&root, recursive, &metadata)?,
        };
        let result = rank(&snapshot, needle, recursive, &root);
        if recursive && !from_cache {
            self.remember(root, snapshot);
        }
        Ok(SearchResult {
            from_cache,
            ..result
        })
    }

    fn cached(&self, root: &Path, metadata: &std::fs::Metadata) -> Option<Snapshot> {
        let snapshot = self
            .snapshots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(root)?
            .clone();
        if snapshot.scanned_at.elapsed() > CACHE_AGE
            || snapshot.root_identity != (metadata.dev(), metadata.ino())
            || snapshot.root_modified != metadata.modified().ok()
        {
            return None;
        }
        Some(snapshot)
    }

    fn remember(&self, root: PathBuf, snapshot: Snapshot) {
        let mut snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if snapshots.len() == MAX_CACHED_ROOTS
            && let Some(oldest) = snapshots
                .iter()
                .min_by_key(|(_, item)| item.scanned_at)
                .map(|(path, _)| path.clone())
        {
            snapshots.remove(&oldest);
        }
        snapshots.insert(root, snapshot);
    }
}

/// Breadth-first traversal gives shallow paths a chance before the budget is
/// spent in a deep subtree. Never descend through symlinks or unreadable dirs.
fn scan(root: &Path, recursive: bool, metadata: &std::fs::Metadata) -> io::Result<Snapshot> {
    // An unreadable root is a refusal, not an empty successful listing.
    std::fs::read_dir(root)?;
    let mut walk = Walk::new(root, recursive);
    while let Some((dir, depth)) = walk.queue.pop_front() {
        let Ok(children) = std::fs::read_dir(dir) else {
            walk.truncated = true;
            continue;
        };
        for child in children {
            if walk.exhausted() {
                walk.truncated = true;
                break;
            }
            walk.visit(child, depth);
        }
        if walk.exhausted() {
            walk.truncated |= !walk.queue.is_empty();
            break;
        }
    }
    Ok(Snapshot {
        entries: walk.entries,
        scanned_at: walk.started,
        truncated: walk.truncated,
        root_identity: (metadata.dev(), metadata.ino()),
        root_modified: metadata.modified().ok(),
    })
}

struct Walk {
    queue: VecDeque<(PathBuf, usize)>,
    entries: Vec<PathHit>,
    visited: usize,
    bytes: usize,
    started: Instant,
    recursive: bool,
    truncated: bool,
}

impl Walk {
    fn new(root: &Path, recursive: bool) -> Self {
        Self {
            queue: VecDeque::from([(root.to_path_buf(), 0)]),
            entries: Vec::new(),
            visited: 0,
            bytes: 0,
            started: Instant::now(),
            recursive,
            truncated: false,
        }
    }

    fn exhausted(&self) -> bool {
        self.visited >= MAX_VISITED
            || self.bytes >= MAX_INDEX_BYTES
            || self.started.elapsed() >= WALK_BUDGET
    }

    fn visit(&mut self, child: io::Result<std::fs::DirEntry>, depth: usize) {
        self.visited += 1;
        let Ok(child) = child else {
            self.truncated = true;
            return;
        };
        let Ok(kind) = child.file_type() else {
            self.truncated = true;
            return;
        };
        let path = child.path();
        let size = path.as_os_str().len();
        if size > 4096 || self.bytes.saturating_add(size) > MAX_INDEX_BYTES {
            self.truncated = true;
            return;
        }
        self.schedule_child(&path, depth, kind.is_dir());
        // A link to a directory is browsable on explicit selection, but a
        // recursive search must not follow it (including cycles).
        let is_dir = kind.is_dir() || (kind.is_symlink() && path.is_dir());
        self.bytes += size;
        self.entries.push(PathHit {
            path,
            is_dir,
            is_symlink: kind.is_symlink(),
        });
    }

    fn schedule_child(&mut self, path: &Path, depth: usize, is_dir: bool) {
        if !self.recursive || !is_dir {
            return;
        }
        if depth < MAX_DEPTH {
            self.queue.push_back((path.to_path_buf(), depth + 1));
        } else {
            self.truncated = true;
        }
    }
}

fn rank(snapshot: &Snapshot, needle: &str, recursive: bool, root: &Path) -> SearchResult {
    let query = needle.to_lowercase();
    let mut matches: Vec<_> = snapshot
        .entries
        .iter()
        .filter_map(|entry| {
            let relative = entry.path.strip_prefix(root).ok()?;
            // Lossy only for scoring — PathHit keeps the original OsStr bytes.
            let name = relative.to_string_lossy();
            let score = if recursive {
                fuzzy_score(&query, &name.to_lowercase())
            } else {
                name.contains(needle).then_some(0)
            };
            score.map(|score| (score, entry))
        })
        .collect();
    matches
        .sort_by(|(a, path_a), (b, path_b)| b.cmp(a).then_with(|| path_a.path.cmp(&path_b.path)));
    let truncated = snapshot.truncated || matches.len() > MAX_RESULTS;
    let hits = matches
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, entry)| entry.clone())
        .collect();
    SearchResult {
        hits,
        truncated,
        from_cache: false,
    }
}

/// A scored subsequence, rewarding consecutive characters and components.
fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    let mut chars = needle.chars();
    let mut wanted = chars.next();
    let mut score = 0;
    let mut previous_match = false;
    let mut previous_char = None;
    for character in haystack.chars() {
        if Some(character) == wanted {
            score += if previous_match { 5 } else { 1 };
            if previous_char.is_none_or(|prior| prior == '/') {
                score += 3;
            }
            wanted = chars.next();
            previous_match = true;
        } else {
            previous_match = false;
        }
        previous_char = Some(character);
    }
    wanted.is_none().then_some(score)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::fs;
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[test]
    fn browse_lists_files_dirs_and_symlinks_without_descending() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/nested.rs"), "").unwrap();
        fs::write(temp.path().join("file with spaces"), "").unwrap();
        symlink(temp.path().join("src"), temp.path().join("alias")).unwrap();
        let result = PathIndex::default().query(temp.path(), "", false).unwrap();
        assert_eq!(result.hits.len(), 3);
        assert!(!result.truncated);
        assert!(
            result
                .hits
                .iter()
                .any(|item| item.path.ends_with("file with spaces") && !item.is_dir)
        );
        assert!(
            result
                .hits
                .iter()
                .any(|item| item.path.ends_with("src") && item.is_dir)
        );
        assert!(
            result
                .hits
                .iter()
                .any(|item| item.path.ends_with("alias") && item.is_symlink && item.is_dir)
        );
    }

    #[test]
    fn fuzzy_search_finds_a_descendant_and_reuses_the_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/remote_picker.rs"), "").unwrap();
        let index = PathIndex::default();
        let first = index.query(temp.path(), "rpr", true).unwrap();
        assert!(!first.from_cache);
        assert_eq!(first.hits.len(), 1);
        let second = index.query(temp.path(), "picker", true).unwrap();
        assert!(second.from_cache);
        assert_eq!(second.hits, first.hits);
    }

    #[test]
    fn symlink_cycles_are_not_descended() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        symlink(temp.path(), temp.path().join("src/again")).unwrap();
        let result = PathIndex::default()
            .query(temp.path(), "again", true)
            .unwrap();
        assert_eq!(result.hits.len(), 1);
        assert!(result.hits[0].is_symlink);
    }

    #[test]
    fn paths_keep_the_requested_spelling() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("real")).unwrap();
        fs::write(temp.path().join("real/file"), "").unwrap();
        symlink(temp.path().join("real"), temp.path().join("alias")).unwrap();
        let result = PathIndex::default()
            .query(&temp.path().join("alias"), "", false)
            .unwrap();
        assert_eq!(result.hits[0].path, temp.path().join("alias/file"));
    }

    #[test]
    fn changed_symlink_target_invalidates_cached_root() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("one"), "").unwrap();
        fs::write(second.join("two"), "").unwrap();
        let alias = temp.path().join("alias");
        symlink(&first, &alias).unwrap();
        let index = PathIndex::default();
        assert_eq!(index.query(&alias, "one", true).unwrap().hits.len(), 1);
        fs::remove_file(&alias).unwrap();
        symlink(&second, &alias).unwrap();
        let result = index.query(&alias, "two", true).unwrap();
        assert_eq!(result.hits.len(), 1);
        assert!(!result.from_cache);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_paths_keep_non_utf8_bytes() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("real")).unwrap();
        fs::write(
            temp.path()
                .join("real")
                .join(std::ffi::OsStr::from_bytes(b"bad\xff")),
            "",
        )
        .unwrap();
        symlink(temp.path().join("real"), temp.path().join("alias")).unwrap();
        let result = PathIndex::default()
            .query(&temp.path().join("alias"), "", false)
            .unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(
            result.hits[0].path.as_os_str().as_bytes(),
            temp.path()
                .join("alias")
                .join(std::ffi::OsStr::from_bytes(b"bad\xff"))
                .as_os_str()
                .as_bytes()
        );
    }

    #[test]
    fn relative_root_is_refused() {
        assert_eq!(
            PathIndex::default()
                .query(Path::new("relative"), "", false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn result_limit_is_visible_and_hidden_entries_are_searchable() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..110 {
            fs::write(temp.path().join(format!("match-{index:03}")), "").unwrap();
        }
        fs::write(temp.path().join(".hidden-match"), "").unwrap();
        let index = PathIndex::default();
        let result = index.query(temp.path(), "match", true).unwrap();
        assert_eq!(result.hits.len(), MAX_RESULTS);
        assert!(result.truncated);
        assert_eq!(
            index
                .query(temp.path(), ".hidden", true)
                .unwrap()
                .hits
                .len(),
            1
        );
    }

    #[test]
    fn local_browse_uses_literal_name_filter_and_absolute_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("abc.txt"), "").unwrap();
        fs::write(temp.path().join("a_b_c.txt"), "").unwrap();
        let index = PathIndex::default();
        let browse = query_local("~", "abc", false, Some(temp.path()), &index).unwrap();
        assert_eq!(browse.root, temp.path().to_str().unwrap());
        assert_eq!(
            browse.parent.as_deref(),
            temp.path().parent().and_then(Path::to_str)
        );
        assert_eq!(browse.rows.len(), 1);
        assert_eq!(
            browse.rows[0].path,
            temp.path().join("abc.txt").to_str().unwrap()
        );
        assert_eq!(browse.status, PathStatus::Complete);
        let search = query_local("~", "abc", true, Some(temp.path()), &index).unwrap();
        assert_eq!(search.rows.len(), 2);
    }

    #[test]
    fn local_query_refuses_missing_root_and_relative_path() {
        let temp = tempfile::tempdir().unwrap();
        let index = PathIndex::default();
        assert_eq!(
            query_local(
                temp.path().join("missing").to_str().unwrap(),
                "a",
                true,
                None,
                &index
            )
            .unwrap_err()
            .code,
            PathErrorCode::NotFound
        );
        assert_eq!(
            query_local("src", "a", false, None, &index)
                .unwrap_err()
                .code,
            PathErrorCode::Other
        );
    }

    #[test]
    fn local_query_never_renders_non_utf8_filename_as_insertable() {
        let temp = tempfile::tempdir().unwrap();
        let index = PathIndex::default();
        fs::write(temp.path().join("safe"), "").unwrap();
        #[cfg(target_os = "linux")]
        fs::write(
            temp.path().join(std::ffi::OsStr::from_bytes(b"bad\xff")),
            "",
        )
        .unwrap();
        let listing = query_local(temp.path().to_str().unwrap(), "", false, None, &index).unwrap();
        assert_eq!(listing.rows.len(), 1);
        assert!(listing.rows[0].path.ends_with("/safe"));
    }

    #[test]
    fn inaccessible_root_is_a_typed_refusal_instead_of_an_empty_listing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("private");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
        let privileged = fs::read_dir(&root).is_ok();
        let result = query_local(
            root.to_str().unwrap(),
            "",
            false,
            None,
            &PathIndex::default(),
        );
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        if !privileged {
            assert_eq!(result.unwrap_err().code, PathErrorCode::PermissionDenied);
        }
    }

    #[test]
    fn depth_bound_reports_a_partial_search() {
        let temp = tempfile::tempdir().unwrap();
        let mut child = temp.path().to_path_buf();
        for _ in 0..=MAX_DEPTH {
            child.push("next");
            fs::create_dir(&child).unwrap();
        }
        let result = PathIndex::default()
            .query(temp.path(), "next", true)
            .unwrap();
        assert!(result.truncated);
    }
}
