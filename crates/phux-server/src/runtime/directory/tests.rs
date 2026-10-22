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
