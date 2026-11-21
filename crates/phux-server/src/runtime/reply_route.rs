//! Recent failures to route a reply to a remote peer.
//!
//! A bound listener and a local probe can both look healthy while this host
//! still cannot send to the peer. The writer records the peer address and a
//! short kind; `phux doctor` reads the file. The record is the route label
//! and the kind only: no certificate, token, path, or error text.

use std::io::{self, Write as _};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::debug;

const FILE_NAME: &str = "reply-route.json";
const TEMP_NAME: &str = ".reply-route.json.tmp";
const VERSION: u32 = 1;
const MAX_RECORDS: usize = 8;
const DEDUPE_MS: u64 = 30_000;

const HOST_UNREACHABLE: &str = "host_unreachable";
const NETWORK_UNREACHABLE: &str = "network_unreachable";
const HOST_DOWN: &str = "host_down";

const NO_ROUTE: &str = "No route to host";
const NETWORK_UNREACHABLE_TEXT: &str = "Network is unreachable";
const HOST_DOWN_TEXT: &str = "Host is down";

static RECORD_LOCK: Mutex<()> = Mutex::new(());

/// One recorded failure to route a reply to a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRouteFailure {
    /// Route label: the peer address, or `unknown peer`.
    pub peer: String,
    /// Reachability class: `host_unreachable`, `network_unreachable`, or `host_down`.
    pub kind: String,
    /// Milliseconds since the unix epoch when the failure was recorded.
    pub at_ms: u64,
}

#[derive(Serialize)]
struct OutFile<'a> {
    version: u32,
    failures: &'a [ReplyRouteFailure],
}

#[derive(Deserialize)]
struct InFile {
    version: u32,
    failures: Vec<ReplyRouteFailure>,
}

/// Reachability class for `err`, if a reply failed because the peer could
/// not be routed to.
///
/// Host-unreachable, network-unreachable, and display text containing
/// "No route to host", "Network is unreachable", or "Host is down" count.
/// Connection resets, broken pipes, timeouts, would-block, and
/// address-not-available do not, even when their text contains one of
/// those phrases.
#[must_use]
pub(crate) fn routing_kind(err: &std::io::Error) -> Option<&'static str> {
    match err.kind() {
        io::ErrorKind::HostUnreachable => Some(HOST_UNREACHABLE),
        io::ErrorKind::NetworkUnreachable => Some(NETWORK_UNREACHABLE),
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::TimedOut
        | io::ErrorKind::WouldBlock
        | io::ErrorKind::AddrNotAvailable => None,
        _ => phrase_label(&err.to_string()),
    }
}

fn phrase_label(text: &str) -> Option<&'static str> {
    if text.contains(NO_ROUTE) {
        Some(HOST_UNREACHABLE)
    } else if text.contains(NETWORK_UNREACHABLE_TEXT) {
        Some(NETWORK_UNREACHABLE)
    } else if text.contains(HOST_DOWN_TEXT) {
        Some(HOST_DOWN)
    } else {
        None
    }
}

/// Record `peer` + `kind` under the state directory.
///
/// A write failure is logged and ignored. The same peer and kind is stored
/// at most once per 30 seconds.
pub(crate) fn note_unreachable(peer: &str, kind: &'static str) {
    if let Err(err) = record(
        &phux_config::instance::state_dir(),
        peer,
        kind,
        unix_now_ms(),
    ) {
        debug!(error = %err, "reply route failure was not recorded");
    }
}

/// Failures in `dir`/`reply-route.json` whose age is at most `max_age_ms`.
///
/// A missing file, a version other than 1, and unreadable json are no
/// failures. `now_ms` and each `at_ms` are milliseconds since the unix epoch.
#[must_use]
pub fn read_recent(dir: &Path, now_ms: u64, max_age_ms: u64) -> Vec<ReplyRouteFailure> {
    load(dir)
        .into_iter()
        .filter(|failure| now_ms.saturating_sub(failure.at_ms) <= max_age_ms)
        .collect()
}

fn record(dir: &Path, peer: &str, kind: &str, now_ms: u64) -> io::Result<()> {
    let _guard = lock();
    let mut failures = load(dir);
    if seen_recently(&failures, peer, kind, now_ms) {
        return Ok(());
    }
    failures.push(ReplyRouteFailure {
        peer: peer.to_owned(),
        kind: kind.to_owned(),
        at_ms: now_ms,
    });
    trim_oldest(&mut failures);
    persist(dir, &failures)
}

fn seen_recently(failures: &[ReplyRouteFailure], peer: &str, kind: &str, now_ms: u64) -> bool {
    failures.iter().any(|failure| {
        failure.peer == peer
            && failure.kind == kind
            && now_ms.saturating_sub(failure.at_ms) < DEDUPE_MS
    })
}

fn trim_oldest(failures: &mut Vec<ReplyRouteFailure>) {
    if failures.len() <= MAX_RECORDS {
        return;
    }
    failures.sort_by_key(|failure| failure.at_ms);
    let overflow = failures.len() - MAX_RECORDS;
    failures.drain(0..overflow);
}

fn load(dir: &Path) -> Vec<ReplyRouteFailure> {
    let Ok(bytes) = std::fs::read(dir.join(FILE_NAME)) else {
        return Vec::new();
    };
    decode(&bytes).unwrap_or_default()
}

fn decode(bytes: &[u8]) -> Option<Vec<ReplyRouteFailure>> {
    let file = serde_json::from_slice::<InFile>(bytes).ok()?;
    (file.version == VERSION).then_some(file.failures)
}

fn persist(dir: &Path, failures: &[ReplyRouteFailure]) -> io::Result<()> {
    crate::owner_dir::create(dir)?;
    let bytes = serde_json::to_vec(&OutFile {
        version: VERSION,
        failures,
    })
    .map_err(io::Error::other)?;
    let tmp = dir.join(TEMP_NAME);
    if let Err(err) = write_temp(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&tmp, dir.join(FILE_NAME)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn write_temp(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    RECORD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn unix_now_ms() -> u64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::PermissionsExt as _;

    use super::{FILE_NAME, read_recent, record, routing_kind};

    #[test]
    fn routing_kind_table() {
        let cases = [
            (
                Error::new(ErrorKind::HostUnreachable, "ignored"),
                Some("host_unreachable"),
            ),
            (
                Error::new(ErrorKind::NetworkUnreachable, "ignored"),
                Some("network_unreachable"),
            ),
            (Error::other("No route to host"), Some("host_unreachable")),
            (
                Error::other("send failed: Network is unreachable"),
                Some("network_unreachable"),
            ),
            (Error::other("Host is down"), Some("host_down")),
            (
                Error::other("write failed: No route to host (os error 65)"),
                Some("host_unreachable"),
            ),
            (
                Error::new(ErrorKind::ConnectionReset, "No route to host"),
                None,
            ),
            (Error::new(ErrorKind::BrokenPipe, "No route to host"), None),
            (Error::new(ErrorKind::TimedOut, "Host is down"), None),
            (
                Error::new(ErrorKind::WouldBlock, "Network is unreachable"),
                None,
            ),
            (
                Error::new(ErrorKind::AddrNotAvailable, "No route to host"),
                None,
            ),
            (Error::other("connection reset"), None),
        ];
        for (err, expected) in cases {
            assert_eq!(routing_kind(&err), expected, "{err}");
        }
    }

    #[test]
    fn record_writes_the_route_label_and_kind_only() {
        let dir = tempfile::tempdir().expect("temp");
        let nested = dir.path().join("state");
        record(&nested, "203.0.113.9:7", "host_unreachable", 10).expect("record");

        let path = nested.join(FILE_NAME);
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(!raw.contains("No route"), "{raw}");
        assert!(!raw.contains("os error"), "{raw}");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(value["version"], 1);
        assert_eq!(value["failures"][0]["peer"], "203.0.113.9:7");
        assert_eq!(value["failures"][0]["kind"], "host_unreachable");
        assert_eq!(value["failures"][0]["at_ms"], 10);
        assert!(value["failures"][0].get("error").is_none());
        assert!(!nested.join(".reply-route.json.tmp").exists());

        let file_mode = std::fs::metadata(&path).expect("file").permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(&nested)
            .expect("dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn read_recent_drops_stale_entries_and_keeps_fresh_ones() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path();
        record(path, "stale", "host_unreachable", 1_000).unwrap();
        record(path, "fresh", "network_unreachable", 50_000).unwrap();

        let recent = read_recent(path, 50_000, 10_000);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].peer, "fresh");
        assert_eq!(recent[0].kind, "network_unreachable");

        assert_eq!(read_recent(path, 50_000, 49_000).len(), 2);
        let dropped = read_recent(path, 50_000, 48_999);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].peer, "fresh");
    }

    #[test]
    fn read_recent_is_empty_for_missing_corrupt_and_unknown_versions() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path();
        assert!(read_recent(path, 0, 1).is_empty());

        let file = path.join(FILE_NAME);
        std::fs::write(&file, b"{").unwrap();
        assert!(read_recent(path, 0, 1).is_empty());

        std::fs::write(
            &file,
            br#"{"version":2,"failures":[{"peer":"203.0.113.1:1","kind":"host_down","at_ms":1}]}"#,
        )
        .unwrap();
        assert!(read_recent(path, 10, 10).is_empty());
    }

    #[test]
    fn identical_peer_and_kind_is_one_record_per_thirty_seconds() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path();
        record(path, "203.0.113.9:7", "host_unreachable", 1_000).unwrap();
        record(path, "203.0.113.9:7", "host_unreachable", 1_000 + 29_999).unwrap();
        assert_eq!(read_recent(path, 40_000, 40_000).len(), 1);

        record(path, "203.0.113.9:7", "host_down", 1_000 + 1).unwrap();
        record(path, "203.0.113.10:7", "host_unreachable", 1_000 + 1).unwrap();
        assert_eq!(read_recent(path, 40_000, 40_000).len(), 3);

        record(path, "203.0.113.9:7", "host_unreachable", 1_000 + 30_000).unwrap();
        assert_eq!(read_recent(path, 40_000, 40_000).len(), 4);
    }

    #[test]
    fn record_keeps_at_most_eight_and_drops_the_oldest_timestamp() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path();
        record(path, "keep", "host_unreachable", 100).unwrap();
        for n in 1_u64..9 {
            record(path, &format!("old-{n}"), "host_down", n).unwrap();
        }
        let recent = read_recent(path, 1_000, 1_000);
        assert_eq!(recent.len(), 8);
        assert!(recent.iter().any(|failure| failure.peer == "keep"));
        assert!(recent.iter().all(|failure| failure.peer != "old-1"));
    }

    #[test]
    fn record_returns_an_error_when_the_directory_cannot_be_created() {
        let dir = tempfile::tempdir().expect("temp");
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"x").unwrap();
        assert!(record(&blocked, "203.0.113.1:1", "host_unreachable", 1).is_err());
    }
}
