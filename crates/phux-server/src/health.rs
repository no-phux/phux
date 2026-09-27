//! Server start history, so a crash-loop is reportable. Each start appends
//! one fixed-format record (not derived from the rotated, unstable
//! `server.log`); `phux doctor` reports the restart rate.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Records retained.
const MAX_RECORDS: usize = 64;

/// Starts within the window that mean a crash-loop (a healthy supervised
/// server starts once).
pub const CRASH_LOOP_THRESHOLD: usize = 5;

/// The window over which restarts are counted.
pub const CRASH_LOOP_WINDOW: Duration = Duration::from_hours(1);

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

/// Append a record for this process (capped); failures are ignored.
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

/// The version of the most recently started server: the only way a client
/// learns the running build, which a package upgrade leaves running.
#[must_use]
pub fn running_version() -> Option<String> {
    read_records(&history_path())
        .pop()
        .map(|record| record.version)
}

/// Whether recent restarts look like a crash-loop, and the count.
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
