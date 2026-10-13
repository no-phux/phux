//! Live marker inbox for an in-progress recording.
//!
//! The process that owns the cast ([`MarkInbox::register`]) creates
//! `{pid}.marks` under the mark directory. `phux rec mark` and the TUI
//! `rec-mark` action append one label per line ([`post_mark`]). The owner
//! polls new lines and writes them as asciicast `m` events. A crashed
//! owner's file is removed the next time a marker is posted, when the
//! platform can tell the pid is gone.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Environment override for the directory that holds live inboxes.
/// Tests pass a private directory; production uses `$XDG_RUNTIME_DIR`.
const MARK_DIR_ENV: &str = "PHUX_REC_MARK_DIR";

/// Where live recording inboxes live.
///
/// `PHUX_REC_MARK_DIR` wins. Otherwise `$XDG_RUNTIME_DIR/phux-rec-marks`,
/// and if that is unset a directory under the temp dir.
#[must_use]
pub fn mark_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(MARK_DIR_ENV) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("phux-rec-marks");
    }
    std::env::temp_dir().join("phux-rec-marks")
}

/// The inbox file a recording process owns.
#[derive(Debug)]
pub struct MarkInbox {
    path: PathBuf,
    /// Byte offset already consumed. Only complete lines are returned;
    /// a trailing partial line stays for the next poll.
    offset: u64,
    /// Bytes of a trailing partial line already read and held back.
    pending: String,
}

impl MarkInbox {
    /// Create this process's inbox, replacing a stale file from a previous
    /// run that reused the pid.
    pub fn register() -> io::Result<Self> {
        Self::register_in(&mark_dir())
    }

    /// [`Self::register`] into an explicit directory.
    pub fn register_in(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.marks", std::process::id()));
        File::create(&path)?;
        Ok(Self {
            path,
            offset: 0,
            pending: String::new(),
        })
    }

    /// Labels appended since the previous poll, in order.
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        self.offset = self
            .offset
            .saturating_add(u64::try_from(buf.len()).unwrap_or(0));
        self.pending.push_str(&buf);
        let mut labels = Vec::new();
        while let Some(at) = self.pending.find('\n') {
            let line = self.pending.drain(..=at).collect::<String>();
            let label = line.trim_end_matches(['\n', '\r']);
            labels.push(label.to_owned());
        }
        Ok(labels)
    }

    /// Remove the inbox. A missing file is fine.
    pub fn remove(self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for MarkInbox {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Append `label` to every live recording inbox. Returns how many
/// recordings were notified.
///
/// A label containing a newline is rejected: the inbox is one label per
/// line. An empty label is a legal asciicast marker.
pub fn post_mark(label: &str) -> io::Result<usize> {
    post_mark_in(&mark_dir(), label)
}

/// [`post_mark`] against an explicit directory.
pub fn post_mark_in(dir: &Path, label: &str) -> io::Result<usize> {
    if label.contains(['\n', '\r']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a marker label cannot contain a newline",
        ));
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    let mut notified = 0_usize;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(pid) = inbox_pid(&path) else {
            continue;
        };
        if !process_alive(pid) {
            let _ = fs::remove_file(&path);
            continue;
        }
        let mut file = OpenOptions::new().append(true).open(&path)?;
        file.write_all(label.as_bytes())?;
        file.write_all(b"\n")?;
        notified = notified.saturating_add(1);
    }
    Ok(notified)
}

/// Pid encoded in `{pid}.marks`, if this path is an inbox.
fn inbox_pid(path: &Path) -> Option<u32> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("marks") {
        return None;
    }
    path.file_stem()?.to_str()?.parse().ok()
}

/// Whether `pid` is still a live process.
///
/// Linux checks `/proc`. Other platforms cannot probe without a new
/// dependency, so a foreign pid is treated as live and its inbox is left
/// for the owner to delete.
fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    if cfg!(target_os = "linux") {
        return Path::new(&format!("/proc/{pid}")).is_dir();
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("phux-marks-{}-{}-{name}", std::process::id(), n));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch dir");
        path
    }

    #[test]
    fn post_and_poll_round_trip_labels_in_order() {
        let dir = scratch("round");
        let mut inbox = MarkInbox::register_in(&dir).expect("register");
        assert_eq!(post_mark_in(&dir, "intro").expect("post"), 1);
        assert_eq!(post_mark_in(&dir, "").expect("empty"), 1);
        assert_eq!(
            inbox.poll().expect("poll"),
            ["intro".to_owned(), String::new()]
        );
        assert!(inbox.poll().expect("second poll").is_empty());
        drop(inbox);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_partial_line_waits_for_its_newline() {
        let dir = scratch("partial");
        let mut inbox = MarkInbox::register_in(&dir).expect("register");
        let path = dir.join(format!("{}.marks", std::process::id()));
        fs::write(&path, "hel").expect("partial");
        assert!(inbox.poll().expect("poll").is_empty());
        let mut file = OpenOptions::new().append(true).open(&path).expect("open");
        file.write_all(b"lo\n").expect("rest");
        assert_eq!(inbox.poll().expect("poll"), ["hello".to_owned()]);
        drop(inbox);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newline_in_the_label_is_rejected() {
        let dir = scratch("newline");
        let _inbox = MarkInbox::register_in(&dir).expect("register");
        let err = post_mark_in(&dir, "a\nb").expect_err("newline");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_notifies_nobody() {
        let missing = std::env::temp_dir().join(format!(
            "phux-marks-missing-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(post_mark_in(&missing, "x").expect("missing"), 0);
    }
}
