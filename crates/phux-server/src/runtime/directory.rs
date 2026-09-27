//! `LIST_DIRECTORY` host query (`docs/spec/L3.md` §4): the child directories
//! of a path on this host, as the server's user.
//!
//! The blocking walk runs on tokio's blocking pool under a deadline and a
//! server-wide in-flight cap, and is bounded by [`MAX_SCANNED_ENTRIES`] and
//! [`MAX_DIRECTORY_ENTRIES`] (either sets `truncated`). A request naming a
//! satellite `host` is relayed by a federation hub under server-wide and
//! per-satellite caps, and refused elsewhere. A client that disconnects
//! abandons its request and releases its permits.
//!
//! Security: nothing here exceeds what a client could learn by spawning a
//! shell as the same user.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError,
    DirectoryListingResult, FrameKind, MAX_DIRECTORY_ENTRIES,
};
use tokio::sync::Semaphore;
use tracing::{debug, trace};

use crate::hub::relay::{RelayHandle, listing_refusal};
use crate::state::{ClientId, Outbound, SharedState};

/// Most raw directory entries one listing reads before reporting truncation.
const MAX_SCANNED_ENTRIES: usize = 16 * 1024;

/// How long the handler waits for the blocking walk before refusing with
/// [`DirectoryErrorCode::Other`].
const LIST_DEADLINE: Duration = Duration::from_secs(5);

/// One decoded `LIST_DIRECTORY`.
pub(super) struct ListRequest {
    pub(super) request_id: u32,
    /// The requested path, verbatim.
    pub(super) path: String,
    /// The satellite to list on, or `None` for this server's own host.
    pub(super) host: Option<SatelliteHost>,
}

/// Where a request is answered: this host's filesystem, a satellite over its
/// hub link, or nowhere (the refusal message says why).
enum ListingRoute {
    Local,
    Relay(RelayHandle),
    Refused(String),
}

/// Dispatch one `LIST_DIRECTORY` for an L3 consumer (`docs/spec/L3.md`
/// §1.2), answering from a spawned task so the frame loop keeps running.
pub(super) fn handle_list_directory(
    state: &SharedState,
    client_id: ClientId,
    request: ListRequest,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let ListRequest {
        request_id,
        path,
        host,
    } = request;
    let speaks_l3 = state.with(|s| s.client_speaks_l3(client_id));
    debug!(
        ?client_id,
        request_id,
        path = log_prefix(&path),
        path_bytes = path.len(),
        host = ?host,
        speaks_l3,
        "LIST_DIRECTORY"
    );
    if !speaks_l3 {
        return;
    }
    let route = listing_route(state, host);
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        // Dropping the answer when the client leaves releases its permits.
        let result = tokio::select! {
            result = answer(route, path) => result,
            () = out_tx.closed() => {
                trace!(?client_id, request_id, "LIST_DIRECTORY abandoned: client gone");
                return;
            }
        };
        let reply = FrameKind::DirectoryListing { request_id, result };
        if out_tx.send(Outbound::Frame(reply)).await.is_err() {
            trace!(
                ?client_id,
                request_id, "DIRECTORY_LISTING send dropped: writer gone"
            );
        }
    });
}

/// Longest request `path` accepted, in bytes; refused before any use.
const MAX_REQUEST_PATH_BYTES: usize = 4096;

/// How much of a request path the debug log records.
const LOG_PATH_BYTES: usize = 256;

/// Most listings holding a blocking-pool thread at once. The deadline cannot
/// free a thread stuck on a hung mount, so this caps how many can leak.
const MAX_LISTINGS_IN_FLIGHT: usize = 8;

/// Permits for [`MAX_LISTINGS_IN_FLIGHT`], server-wide across connections.
static LISTINGS: Semaphore = Semaphore::const_new(MAX_LISTINGS_IN_FLIGHT);

/// Most relayed listings waiting on satellites at once, server-wide.
const MAX_RELAYED_LISTINGS_IN_FLIGHT: usize = 8;

/// Permits for [`MAX_RELAYED_LISTINGS_IN_FLIGHT`].
pub(super) static RELAYED_LISTINGS: Semaphore =
    Semaphore::const_new(MAX_RELAYED_LISTINGS_IN_FLIGHT);

/// Most relayed listings one satellite may hold, so a silent satellite cannot
/// starve healthy ones of the server-wide pool.
const MAX_RELAYED_LISTINGS_PER_HOST: usize = 2;

/// Per-satellite counts for [`MAX_RELAYED_LISTINGS_PER_HOST`].
pub(super) static RELAYED_PER_HOST: HostSlots = HostSlots::new(MAX_RELAYED_LISTINGS_PER_HOST);

/// In-flight relayed listings per satellite, capped at `per_host` each.
pub(super) struct HostSlots {
    per_host: usize,
    in_flight: Mutex<BTreeMap<SatelliteHost, usize>>,
}

impl HostSlots {
    const fn new(per_host: usize) -> Self {
        Self {
            per_host,
            in_flight: Mutex::new(BTreeMap::new()),
        }
    }

    /// Take one of `host`'s slots, or `None` when it already holds all of
    /// them. The slot returns when the permit drops.
    pub(super) fn try_acquire(&'static self, host: &SatelliteHost) -> Option<HostPermit> {
        let mut in_flight = self.lock();
        let held = in_flight.get(host).copied().unwrap_or(0);
        if held >= self.per_host {
            return None;
        }
        in_flight.insert(host.clone(), held + 1);
        drop(in_flight);
        Some(HostPermit {
            slots: self,
            host: host.clone(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<SatelliteHost, usize>> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// One held per-satellite slot; dropping it returns the slot and forgets a
/// host with none left.
pub(super) struct HostPermit {
    slots: &'static HostSlots,
    host: SatelliteHost,
}

impl Drop for HostPermit {
    fn drop(&mut self) {
        let mut in_flight = self.slots.lock();
        let remaining = in_flight.get_mut(&self.host).map(|held| {
            *held = held.saturating_sub(1);
            *held
        });
        if remaining == Some(0) {
            in_flight.remove(&self.host);
        }
    }
}

/// Resolve where a request is answered (`docs/spec/L3.md` §4.1).
fn listing_route(state: &SharedState, host: Option<SatelliteHost>) -> ListingRoute {
    let Some(host) = host else {
        return ListingRoute::Local;
    };
    state.with(|s| match s.hub_relay(&host) {
        Some(relay) => ListingRoute::Relay(relay),
        None if s.hub_table().is_some() => {
            ListingRoute::Refused(format!("no satellite named {host} in this hub's registry"))
        }
        None => ListingRoute::Refused(format!(
            "this server is not a federation hub; it has no route to satellite {host}"
        )),
    })
}

/// Produce the reply for one request along its route.
async fn answer(route: ListingRoute, path: String) -> DirectoryListingResult {
    match route {
        ListingRoute::Local => list_request(path).await,
        ListingRoute::Relay(relay) => {
            relay_request(&RELAYED_LISTINGS, &RELAYED_PER_HOST, &relay, path).await
        }
        // L3 §4.1: a refusal carries the full requested path.
        ListingRoute::Refused(message) => Err(listing_refusal(&path, message)),
    }
}

/// Refuse a path over [`MAX_REQUEST_PATH_BYTES`].
fn check_path_len(path: &str) -> Result<(), DirectoryListingError> {
    if path.len() > MAX_REQUEST_PATH_BYTES {
        return Err(other(
            log_prefix(path),
            format!("path exceeds {MAX_REQUEST_PATH_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// Refuse an oversized path, then relay under the per-satellite and
/// server-wide caps, holding the permits until the relay settles.
async fn relay_request(
    slots: &'static Semaphore,
    hosts: &'static HostSlots,
    relay: &RelayHandle,
    path: String,
) -> DirectoryListingResult {
    check_path_len(&path)?;
    let Some(_host_permit) = hosts.try_acquire(relay.host()) else {
        return Err(listing_refusal(
            &path,
            format!(
                "satellite {} already has {} directory listings in flight; retry",
                relay.host(),
                hosts.per_host
            ),
        ));
    };
    let Ok(_permit) = slots.try_acquire() else {
        return Err(listing_refusal(
            &path,
            format!(
                "too many relayed directory listings in flight; satellite {} was not asked",
                relay.host()
            ),
        ));
    };
    relay.list_directory(path).await
}

/// Refuse an oversized path, then list under the server-wide cap.
async fn list_request(path: String) -> DirectoryListingResult {
    check_path_len(&path)?;
    list_bounded(&LISTINGS, path).await
}

/// Run [`list_directory`] on the blocking pool under [`LIST_DEADLINE`]. The
/// `slots` permit is released when the walk finishes, not at the deadline.
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
pub(super) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Resolve `request` (empty, `~`, `~/rest`, or absolute) and list its child
/// directories. Blocking. The path is normalized lexically, not
/// canonicalized, so it reports what the user navigated.
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

pub(super) fn resolve_request_path(
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

    /// Both the entry bound (keeping the first names) and the scan bound set
    /// `truncated`.
    #[test]
    fn truncates_at_the_entry_and_scan_bounds() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["e", "d", "c", "b", "a"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }
        let path = tmp.path().to_str().unwrap();

        let by_entries = list_directory(path, None, 3, 1024).unwrap();
        assert_eq!(names(&by_entries), ["a", "b", "c"]);
        assert!(by_entries.truncated);

        let by_scan = list_directory(path, None, 1024, 2).unwrap();
        assert_eq!(by_scan.entries.len(), 2);
        assert!(by_scan.truncated);
    }

    #[test]
    fn io_errors_map_to_their_codes() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            error_code(list(&tmp.path().join("nope"))),
            DirectoryErrorCode::NotFound
        );

        let file = tmp.path().join("file.txt");
        fs::write(&file, b"x").unwrap();
        let refusal = list(&file).unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::NotADirectory);
        assert_eq!(refusal.path, file.to_str().unwrap());

        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root can read a mode-000 directory, so there is no denial to see.
        let privileged = fs::read_dir(&locked).is_ok();
        let result = list(&locked);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        if !privileged {
            assert_eq!(error_code(result), DirectoryErrorCode::PermissionDenied);
        }
    }

    /// Home forms expand against the given home; without one, and for a
    /// relative path, the request is refused.
    #[test]
    fn request_path_forms() {
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

        assert_eq!(
            error_code(list_directory("~", None, 1024, 1024)),
            DirectoryErrorCode::Other
        );
        let refusal = list_directory("src/lib", None, 1024, 1024).unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        assert_eq!(refusal.path, "src/lib");
    }

    #[test]
    fn dot_segments_normalize_lexically_and_the_root_has_no_parent() {
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

        let root = list_directory("/", None, 1024, 1024).unwrap();
        assert_eq!((root.path.as_str(), root.parent), ("/", None));
    }

    /// The off-runtime walk returns its result and hands its permit back; a
    /// full pool is refused as busy.
    #[tokio::test]
    async fn bounded_listing_returns_permits_and_refuses_when_full() {
        static ONE: Semaphore = Semaphore::const_new(1);
        static EXHAUSTED: Semaphore = Semaphore::const_new(0);
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("child")).unwrap();
        let path = tmp.path().to_str().unwrap().to_owned();

        for _ in 0..2 {
            let listing = list_bounded(&ONE, path.clone()).await.unwrap();
            assert_eq!(names(&listing), ["child"]);
        }
        assert_eq!(ONE.available_permits(), 1);

        let refusal = list_bounded(&EXHAUSTED, "/".to_owned()).await.unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        assert_eq!(refusal.message, "too many directory listings in flight");
    }

    /// An overlong path is refused before the filesystem or the link, and
    /// only its log prefix (cut on a character boundary) is echoed.
    #[tokio::test]
    async fn an_overlong_path_is_refused_before_the_filesystem_or_link() {
        static ONE: Semaphore = Semaphore::const_new(1);
        static HOSTS: HostSlots = HostSlots::new(2);
        let path = format!("/{}", "a".repeat(MAX_REQUEST_PATH_BYTES));

        let refusal = list_request(path.clone()).await.unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        assert!(refusal.message.contains("exceeds 4096 bytes"));
        assert_eq!(refusal.path.len(), LOG_PATH_BYTES);

        let (relay, mut mailbox) = RelayHandle::new(SatelliteHost::new("devbox"));
        let refusal = relay_request(&ONE, &HOSTS, &relay, path).await.unwrap_err();
        assert!(refusal.message.contains("exceeds 4096 bytes"));
        assert!(
            mailbox.requests.try_recv().is_err(),
            "nothing reached the link"
        );

        let wide = "é".repeat(LOG_PATH_BYTES);
        assert!(log_prefix(&wide).len() <= LOG_PATH_BYTES);
        assert!(log_prefix(&wide).chars().all(|c| c == 'é'));
        assert_eq!(log_prefix("/short"), "/short");
    }

    /// A refused route names the host and keeps the full requested path.
    #[tokio::test]
    async fn a_refused_route_keeps_the_full_path_and_names_the_host() {
        let path = format!("/{}", "d".repeat(LOG_PATH_BYTES * 2));
        let refusal = answer(
            ListingRoute::Refused("no satellite named ghost in this hub's registry".to_owned()),
            path.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        assert_eq!(refusal.path, path, "only logging truncates");
        assert!(refusal.message.contains("ghost"));
    }

    #[tokio::test]
    async fn a_relayed_listing_is_refused_when_the_relay_pool_is_full() {
        static EXHAUSTED: Semaphore = Semaphore::const_new(0);
        static HOSTS: HostSlots = HostSlots::new(2);
        let (relay, _mailbox) = RelayHandle::new(SatelliteHost::new("devbox"));
        let refusal = relay_request(&EXHAUSTED, &HOSTS, &relay, "/srv".to_owned())
            .await
            .unwrap_err();
        assert_eq!(refusal.code, DirectoryErrorCode::Other);
        assert!(
            refusal.message.contains("devbox was not asked"),
            "{}",
            refusal.message
        );
    }

    #[tokio::test]
    async fn one_saturated_satellite_does_not_refuse_another() {
        use crate::hub::relay::RelayRequest;

        static POOL: Semaphore = Semaphore::const_new(8);
        static HOSTS: HostSlots = HostSlots::new(2);
        let (hung, _hung_mailbox) = RelayHandle::new(SatelliteHost::new("hung"));
        let first = HOSTS.try_acquire(hung.host()).unwrap();
        let second = HOSTS.try_acquire(hung.host()).unwrap();

        let refused = relay_request(&POOL, &HOSTS, &hung, "/".to_owned())
            .await
            .unwrap_err();
        assert!(
            refused.message.contains("hung already has 2"),
            "{}",
            refused.message
        );
        assert_eq!(
            POOL.available_permits(),
            8,
            "the shared pool was not touched"
        );

        let (healthy, mut mailbox) = RelayHandle::new(SatelliteHost::new("healthy"));
        let satellite = async {
            let Some(RelayRequest::ListDirectory { path, reply }) = mailbox.requests.recv().await
            else {
                panic!("the healthy satellite must be asked");
            };
            reply
                .send(Ok(DirectoryListing {
                    path,
                    parent: None,
                    entries: Vec::new(),
                    truncated: false,
                }))
                .unwrap();
        };
        let (listing, ()) = tokio::join!(
            relay_request(&POOL, &HOSTS, &healthy, "/srv".to_owned()),
            satellite
        );
        assert_eq!(listing.unwrap().path, "/srv");

        drop((first, second));
        assert!(HOSTS.lock().is_empty(), "returned slots forget the host");
    }
}
