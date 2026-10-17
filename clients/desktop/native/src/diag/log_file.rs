//! The bounded log file and the recent-problems ring.
//!
//! The log keeps at most [`LOG_CAP`] bytes in the live file and one `.1`
//! backup beside it, so the app never fills a disk however long it runs.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Bytes the live log may hold before it rotates to `<name>.1`.
pub const LOG_CAP: u64 = 8 * 1024 * 1024;

/// Warnings and errors a diagnostic receipt carries.
pub const RECENT_CAP: usize = 20;

/// Longest recent-problem line kept, in bytes.
const RECENT_LINE_CAP: usize = 512;

/// An append-only log that rotates once it would pass `cap` bytes.
pub struct LogFile {
    path: PathBuf,
    file: Option<File>,
    len: u64,
    cap: u64,
}

impl LogFile {
    /// Open (or create) `path` for appending, under `cap` bytes.
    pub fn open(path: PathBuf, cap: u64) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = append(&path)?;
        let len = file.metadata().map_or(0, |metadata| metadata.len());
        Ok(Self {
            path,
            file: Some(file),
            len,
            cap,
        })
    }

    /// The single backup a rotation leaves: `<name>.1`.
    #[must_use]
    pub fn backup_path(path: &Path) -> PathBuf {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".1");
        path.with_file_name(name)
    }

    /// Append one line, rotating first when it would pass the cap. A rotation
    /// whose rename fails keeps appending to the live file, so nothing logged
    /// is lost; logging never stops the app.
    pub fn write_line(&mut self, line: &[u8]) {
        let incoming = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if self.len > 0 && self.len.saturating_add(incoming) > self.cap {
            self.rotate();
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if file.write_all(line).is_ok() {
            self.len = self.len.saturating_add(incoming);
        }
    }

    /// Move the live file to the backup and start a new one. Only a rename
    /// that succeeded may start the new file: truncating after a failed
    /// rename would wipe the live log with no backup, so that path keeps
    /// appending to the file it has.
    fn rotate(&mut self) {
        if std::fs::rename(&self.path, Self::backup_path(&self.path)).is_err() {
            return;
        }
        // The old handle follows the renamed backup; a fresh file takes the name.
        match append(&self.path) {
            Ok(file) => {
                self.file = Some(file);
                self.len = 0;
            }
            // Writing on through the old handle would grow the backup without
            // bound; stderr still carries every line.
            Err(_) => self.file = None,
        }
    }
}

fn append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// The last [`RECENT_CAP`] warnings and errors, already redacted.
pub struct Recent {
    lines: Mutex<VecDeque<String>>,
}

impl Recent {
    pub const fn new() -> Self {
        Self {
            lines: Mutex::new(VecDeque::new()),
        }
    }

    /// Keep `line` (trimmed and bounded), dropping the oldest past the cap.
    pub fn note(&self, line: &str) {
        let mut line = line.trim_end().to_owned();
        if line.len() > RECENT_LINE_CAP {
            let mut end = RECENT_LINE_CAP;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
            line.push('…');
        }
        let mut lines = self
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if lines.len() == RECENT_CAP {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// Oldest first.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "phux-desktop-log-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("phux-desktop.log")
    }

    #[test]
    fn the_log_rotates_to_one_backup_at_the_cap() {
        let path = scratch("rotate");
        let backup = LogFile::backup_path(&path);
        assert_eq!(backup.file_name().unwrap(), "phux-desktop.log.1");
        let mut log = LogFile::open(path.clone(), 64).unwrap();
        let line = [b'a'; 31];
        log.write_line(&line);
        log.write_line(&line);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 62);
        assert!(!backup.exists(), "under the cap nothing rotates");
        log.write_line(b"third\n");
        assert_eq!(std::fs::metadata(&backup).unwrap().len(), 62);
        assert_eq!(std::fs::read(&path).unwrap(), b"third\n");
        for _ in 0..10 {
            log.write_line(&line);
        }
        assert!(std::fs::metadata(&path).unwrap().len() <= 64);
        assert!(std::fs::metadata(&backup).unwrap().len() <= 64);
        let siblings = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(siblings, 2, "one live file and one backup, never more");
    }

    #[test]
    fn a_reopened_log_counts_what_is_already_there() {
        let path = scratch("reopen");
        std::fs::write(&path, [b'x'; 60]).unwrap();
        let mut log = LogFile::open(path.clone(), 64).unwrap();
        log.write_line(b"0123456789\n");
        assert_eq!(std::fs::read(&path).unwrap(), b"0123456789\n");
        assert_eq!(
            std::fs::metadata(LogFile::backup_path(&path))
                .unwrap()
                .len(),
            60
        );
    }

    #[test]
    fn a_failed_rotation_keeps_the_live_log() {
        let path = scratch("stuck");
        let backup = LogFile::backup_path(&path);
        // A non-empty directory where the backup goes: the rename must fail.
        std::fs::create_dir_all(backup.join("occupied")).unwrap();
        let mut log = LogFile::open(path.clone(), 16).unwrap();
        log.write_line(b"first line....\n");
        log.write_line(b"second line...\n");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"first line....\nsecond line...\n",
            "the live log is never truncated without a backup"
        );
        assert!(backup.join("occupied").is_dir());
    }

    #[test]
    fn recent_problems_are_bounded() {
        let recent = Recent::new();
        for index in 0..(RECENT_CAP + 5) {
            recent.note(&format!("problem {index}\n"));
        }
        let lines = recent.snapshot();
        assert_eq!(lines.len(), RECENT_CAP);
        assert_eq!(lines[0], "problem 5");
        recent.note(&"é".repeat(RECENT_LINE_CAP));
        assert!(recent.snapshot().last().unwrap().len() <= RECENT_LINE_CAP + 3);
    }
}
