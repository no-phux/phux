//! Server start history, so a crash-loop is a reportable fact (phux-zomb.6).
//!
//! A supervised server that dies and is restarted looks, from outside,
//! exactly like a server that has been up the whole time: the socket answers,
//! `phux ls` works, and the only trace is another startup line buried in a
//! log nobody reads. That is how a genuinely broken server went unnoticed on
//! a developer machine for weeks while accumulating 1487 generations.
//!
//! Supervision is worth keeping (ADR-0080) — but only alongside something
//! that can say "this server keeps dying." Each server start appends one
//! record here; `phux doctor` reads them back and reports the restart rate.
//!
//! Deliberately not derived from `server.log`: that file is rotated, is
//! written by a `tracing` formatter whose shape is not a stable contract, and
//! may carry ANSI escapes. A purpose-built, append-only, fixed-format file is
//! both cheaper to read and impossible to misparse.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many records are retained. Comfortably more than the restart count
/// that constitutes a crash-loop, and small enough that reading the whole
/// file is free.
const MAX_RECORDS: usize = 64;

/// Restarts within this window that constitute a crash-loop.
///
/// With the supervisor's 30s throttle a healthy server starts once and stays
/// up; five starts in an hour cannot happen without something killing it.
pub const CRASH_LOOP_THRESHOLD: usize = 5;

/// The window over which restarts are counted.
pub const CRASH_LOOP_WINDOW: Duration = Duration::from_secs(60 * 60);

/// One server generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRecord {
    /// Seconds since the Unix epoch.
    pub at: u64,
    /// The server process's pid, matching the `server.log` startup line.
    pub pid: u32,
    /// The `phux` version that started, so an upgrade is visible in the history.
    pub version: String,
}

/// Where the start history lives.
#[must_use]
pub fn history_path() -> PathBuf {
    crate::telemetry::state_dir().join("server-starts.log")
}

/// Append a record for this process, trimming the file to a fixed cap.
///
/// Every failure is swallowed: health bookkeeping must never be the reason a
/// server refuses to start.
pub fn record_start(pid: u32, version: &str) {
    let path = history_path();
    let Some(now) = epoch_secs() else { return };
    let mut records = read_records(&path);
    records.push(StartRecord {
        at: now,
        pid,
        version: version.to_owned(),
    });
    // Keep the tail: the recent past is what "is it crash-looping *now*"
    // depends on.
    let start = records.len().saturating_sub(MAX_RECORDS);
    write_records(&path, &records[start..]);
}

/// Records written within `window` of now, oldest first.
#[must_use]
pub fn recent_starts(window: Duration) -> Vec<StartRecord> {
    let Some(now) = epoch_secs() else {
        return Vec::new();
    };
    let cutoff = now.saturating_sub(window.as_secs());
    read_records(&history_path())
        .into_iter()
        .filter(|record| record.at >= cutoff)
        .collect()
}

/// The version of the most recently started server, if one is recorded.
///
/// The wire handshake negotiates the *protocol* version, not the binary's, so
/// this history is the only place a client can learn which build is actually
/// serving it. That matters because the common upgrade path — a package
/// manager replacing the binary — leaves the running server on the old build
/// indefinitely: nothing restarts it, and the socket keeps answering
/// (phux-zomb.7).
#[must_use]
pub fn running_version() -> Option<String> {
    read_records(&history_path())
        .pop()
        .map(|record| record.version)
}

/// Whether the recent restart rate looks like a crash-loop, and the count.
///
/// Returns `None` when the server is starting normally, so a caller can treat
/// `Some` as "there is something to report".
#[must_use]
pub fn crash_loop() -> Option<usize> {
    let count = recent_starts(CRASH_LOOP_WINDOW).len();
    (count >= CRASH_LOOP_THRESHOLD).then_some(count)
}

/// Seconds since the Unix epoch, or `None` on a clock before 1970.
fn epoch_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Parse the history file. Unparseable lines are skipped rather than fatal —
/// a corrupt record must not blind the check that reads the rest.
fn read_records(path: &Path) -> Vec<StartRecord> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    body.lines().filter_map(parse_record).collect()
}

/// `<epoch-secs> <pid> <version>`; the version may contain spaces only if a
/// future version string does, so it takes the rest of the line.
fn parse_record(line: &str) -> Option<StartRecord> {
    let mut parts = line.splitn(3, ' ');
    let at = parts.next()?.parse().ok()?;
    let pid = parts.next()?.parse().ok()?;
    let version = parts.next()?.trim().to_owned();
    Some(StartRecord { at, pid, version })
}

/// Rewrite the file with exactly `records`, owner-only.
fn write_records(path: &Path, records: &[StartRecord]) {
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let mut body = String::with_capacity(records.len() * 40);
    for record in records {
        let _ = writeln!(body, "{} {} {}", record.at, record.pid, record.version);
    }
    // Whole-file rewrite rather than append: trimming to MAX_RECORDS needs it,
    // and the file is at most a few KiB.
    let _ = std::fs::write(path, body);
    harden(path);
}

/// Match the log sink's owner-only permissions (ADR-0028): this file records
/// when a user's machine was running, which is not group- or world-business.
#[cfg(unix)]
fn harden(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips() {
        let parsed = parse_record("1700000000 4242 0.13.0").expect("parse");
        assert_eq!(
            parsed,
            StartRecord {
                at: 1_700_000_000,
                pid: 4242,
                version: "0.13.0".to_owned(),
            }
        );
    }

    #[test]
    fn a_corrupt_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server-starts.log");
        std::fs::write(&path, "garbage\n1700000000 1 0.13.0\nalso bad\n").expect("write");
        let records = read_records(&path);
        assert_eq!(records.len(), 1, "the one valid record must survive");
        assert_eq!(records[0].pid, 1);
    }

    #[test]
    fn history_is_trimmed_to_the_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server-starts.log");
        let records: Vec<StartRecord> = (0..u32::try_from(MAX_RECORDS + 20).expect("fits"))
            .map(|i| StartRecord {
                at: 1_700_000_000 + u64::from(i),
                pid: i,
                version: "0.13.0".to_owned(),
            })
            .collect();
        let start = records.len().saturating_sub(MAX_RECORDS);
        write_records(&path, &records[start..]);

        let read_back = read_records(&path);
        assert_eq!(read_back.len(), MAX_RECORDS);
        assert_eq!(
            read_back.last().expect("last").at,
            records.last().expect("last").at,
            "trimming must drop the OLDEST records, not the newest",
        );
    }

    #[test]
    fn missing_history_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_records(&dir.path().join("nope.log")).is_empty());
    }
}
