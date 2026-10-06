//! Bounded on-disk ring of recent plugin action and hook runs.
//!
//! Plugin actions run in whichever process asks (the TUI, `phux config run`,
//! the MCP server) and hooks run in the server, so the ring is a JSON Lines
//! file in the per-profile state directory rather than one process's memory:
//! every runner appends, and `phux plugin log` reads it without a server
//! round trip. Writers hold an exclusive advisory lock while appending; once
//! the file passes [`COMPACT_AT_BYTES`] the writer rewrites it in place down
//! to the newest [`RUN_LOG_CAPACITY`] records. Readers never return more than
//! [`RUN_LOG_CAPACITY`] records either, so the visible log is always bounded.
//! Captured stdout and stderr keep only their last [`OUTPUT_LIMIT_BYTES`].
//!
//! Recording is best effort: a run never fails because its log line could
//! not be written.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{
    CommandSpecOutput, PluginActionError, PluginActionOutcome, PluginActionOutput,
    PluginActionRequest,
};

/// Most records the log keeps and returns.
pub const RUN_LOG_CAPACITY: usize = 100;

/// Captured stdout/stderr kept per record: the trailing bytes, where errors
/// usually are.
pub const OUTPUT_LIMIT_BYTES: usize = 4096;

/// File size past which an append compacts the log to [`RUN_LOG_CAPACITY`].
pub const COMPACT_AT_BYTES: u64 = 1024 * 1024;

/// File name of the log inside the state directory.
pub const RUN_LOG_FILE: &str = "plugin-runs.jsonl";

/// What kind of run a record describes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// A plugin manifest `[[actions]]` entry.
    Action,
    /// A config `[[hooks.*]]` entry or a plugin `[[events]]` hook.
    Hook,
}

/// One recorded run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    /// When the run finished, in milliseconds since the Unix epoch.
    pub at_unix_ms: u64,
    /// Action or hook.
    pub kind: RunKind,
    /// Owning plugin id; `None` for a config hook.
    pub plugin_id: Option<String>,
    /// Action id for an action; the hook label (`hooks.<event>[i]` or
    /// `plugin.<id>.<event-id>`) for a hook.
    pub name: String,
    /// Executed argv; empty when the run failed before a command resolved.
    pub command: Vec<String>,
    /// Completed or timed out; `None` when the process never ran.
    pub outcome: Option<PluginActionOutcome>,
    /// Process exit code, when the OS provided one.
    pub exit_code: Option<i32>,
    /// Wall-clock runtime in milliseconds.
    pub duration_ms: u64,
    /// Tail of captured stdout (see [`OUTPUT_LIMIT_BYTES`]).
    pub stdout: String,
    /// Tail of captured stderr (see [`OUTPUT_LIMIT_BYTES`]).
    pub stderr: String,
    /// Why the run never produced output (config, manifest, or spawn error).
    pub error: Option<String>,
}

impl RunRecord {
    /// Record a plugin action result, including pre-exec failures.
    #[must_use]
    pub fn from_action(
        request: &PluginActionRequest,
        result: &Result<PluginActionOutput, PluginActionError>,
    ) -> Self {
        let mut record = Self::empty(
            RunKind::Action,
            Some(request.plugin_id.clone()),
            request.action_id.clone(),
        );
        match result {
            Ok(output) => {
                record.command.clone_from(&output.command);
                record.fill_output(
                    output.outcome,
                    output.exit_code,
                    output.duration_ms,
                    &output.stdout,
                    &output.stderr,
                );
            }
            Err(err) => record.error = Some(err.to_string()),
        }
        record
    }

    /// Record one hook child run.
    #[must_use]
    pub fn from_hook(
        label: &str,
        plugin_id: Option<&str>,
        command: &[String],
        result: &io::Result<CommandSpecOutput>,
    ) -> Self {
        let mut record = Self::empty(
            RunKind::Hook,
            plugin_id.map(str::to_owned),
            label.to_owned(),
        );
        record.command = command.to_vec();
        match result {
            Ok(output) => record.fill_output(
                output.outcome,
                output.exit_code,
                output.duration_ms,
                &output.stdout,
                &output.stderr,
            ),
            Err(err) => record.error = Some(err.to_string()),
        }
        record
    }

    /// Whether the run completed with exit code 0.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.outcome == Some(PluginActionOutcome::Completed) && self.exit_code == Some(0)
    }

    fn empty(kind: RunKind, plugin_id: Option<String>, name: String) -> Self {
        Self {
            at_unix_ms: now_unix_ms(),
            kind,
            plugin_id,
            name,
            command: Vec::new(),
            outcome: None,
            exit_code: None,
            duration_ms: 0,
            stdout: String::new(),
            stderr: String::new(),
            error: None,
        }
    }

    fn fill_output(
        &mut self,
        outcome: PluginActionOutcome,
        exit_code: Option<i32>,
        duration_ms: u128,
        stdout: &str,
        stderr: &str,
    ) {
        self.outcome = Some(outcome);
        self.exit_code = exit_code;
        self.duration_ms = u64::try_from(duration_ms).unwrap_or(u64::MAX);
        self.stdout = truncate_tail(stdout, OUTPUT_LIMIT_BYTES);
        self.stderr = truncate_tail(stderr, OUTPUT_LIMIT_BYTES);
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Keep the last `limit` bytes of `text` (on a char boundary), prefixed by
/// a marker naming how much was dropped.
#[must_use]
pub fn truncate_tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[... {start} bytes truncated]\n{}", &text[start..])
}

/// The log for the current profile: `<state dir>/plugin-runs.jsonl`.
#[must_use]
pub fn default_path() -> PathBuf {
    phux_config::instance::state_dir().join(RUN_LOG_FILE)
}

/// Append `record` to the current profile's log ([`default_path`]); see
/// [`record_at`].
///
/// # Errors
///
/// As [`record_at`].
pub fn record(record: &RunRecord) -> Result<(), String> {
    record_at(&default_path(), record)
}

/// Append `record` to the log at `path`, refusing production state from a
/// development build.
///
/// # Errors
///
/// The production-state refusal or the I/O failure, as a message.
pub fn record_at(path: &Path, record: &RunRecord) -> Result<(), String> {
    phux_config::production::refuse_dev_on_production_state(path)?;
    append(path, record).map_err(|err| format!("{}: {err}", path.display()))
}

/// [`record_at`] off the async executor; failures are dropped (best effort).
pub async fn record_async(path: PathBuf, run: RunRecord) {
    let _ = tokio::task::spawn_blocking(move || record_at(&path, &run)).await;
}

/// Append `record` to the log at `path`, compacting it once it grows past
/// [`COMPACT_AT_BYTES`].
///
/// # Errors
///
/// When the file cannot be created, locked, written, or compacted.
pub fn append(path: &Path, record: &RunRecord) -> io::Result<()> {
    append_bounded(path, record, RUN_LOG_CAPACITY, COMPACT_AT_BYTES)
}

fn append_bounded(
    path: &Path,
    record: &RunRecord,
    capacity: usize,
    compact_at: u64,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(record).map_err(io::Error::other)?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    file.lock()?;
    file.write_all(line.as_bytes())?;
    if file.metadata()?.len() > compact_at {
        compact(&mut file, capacity)?;
    }
    Ok(())
}

/// Rewrite the locked `file` in place with only its newest `capacity` lines.
fn compact(file: &mut File, capacity: usize) -> io::Result<()> {
    let mut text = String::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_string(&mut text)?;
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    let keep = &lines[lines.len().saturating_sub(capacity)..];
    let mut rewritten = keep.join("\n");
    rewritten.push('\n');
    // Append mode writes at the end, which is offset 0 after truncation.
    file.set_len(0)?;
    file.write_all(rewritten.as_bytes())
}

/// The newest [`RUN_LOG_CAPACITY`] records at `path`, oldest first. A
/// missing file is an empty log; unparseable lines are skipped.
///
/// # Errors
///
/// When the file exists but cannot be opened, locked, or read.
pub fn read(path: &Path) -> io::Result<Vec<RunRecord>> {
    read_bounded(path, RUN_LOG_CAPACITY)
}

fn read_bounded(path: &Path, capacity: usize) -> io::Result<Vec<RunRecord>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    file.lock_shared()?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let mut records: Vec<RunRecord> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let excess = records.len().saturating_sub(capacity);
    records.drain(..excess);
    Ok(records)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn sample(name: &str) -> RunRecord {
        let output = CommandSpecOutput {
            outcome: PluginActionOutcome::Completed,
            exit_code: Some(1),
            stdout: "out\n".to_owned(),
            stderr: "boom\n".to_owned(),
            duration_ms: 12,
        };
        RunRecord::from_hook(name, Some("example.p"), &["sh".to_owned()], &Ok(output))
    }

    #[test]
    fn truncate_tail_keeps_the_end_on_a_char_boundary() {
        assert_eq!(truncate_tail("short", 10), "short");
        assert_eq!(
            truncate_tail("0123456789", 4),
            "[... 6 bytes truncated]\n6789"
        );
        // "é" is two bytes; a cut inside it moves forward to the boundary.
        let text = "aé1234";
        let kept = truncate_tail(text, 5);
        assert_eq!(kept, "[... 3 bytes truncated]\n1234");
    }

    #[test]
    fn records_truncate_captured_output() {
        let output = CommandSpecOutput {
            outcome: PluginActionOutcome::Completed,
            exit_code: Some(0),
            stdout: "x".repeat(OUTPUT_LIMIT_BYTES * 2),
            stderr: String::new(),
            duration_ms: 1,
        };
        let record = RunRecord::from_hook("hooks.pane-exit[0]", None, &[], &Ok(output));
        assert!(record.stdout.starts_with("[... 4096 bytes truncated]\n"));
        assert!(record.stdout.ends_with(&"x".repeat(OUTPUT_LIMIT_BYTES)));
        assert!(record.succeeded());
    }

    #[test]
    fn action_errors_record_the_message_and_never_succeed() {
        let request = PluginActionRequest {
            plugin_id: "example.p".to_owned(),
            action_id: "go".to_owned(),
            timeout: None,
            cwd: None,
        };
        let record = RunRecord::from_action(
            &request,
            &Err(PluginActionError::PluginNotFound("example.p".to_owned())),
        );
        assert_eq!(record.kind, RunKind::Action);
        assert_eq!(record.outcome, None);
        assert_eq!(
            record.error.as_deref(),
            Some("plugin \"example.p\" is not configured")
        );
        assert!(!record.succeeded());
    }

    #[test]
    fn missing_log_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("none.jsonl")).unwrap().is_empty());
    }

    #[test]
    fn append_then_read_round_trips_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(RUN_LOG_FILE);
        append(&path, &sample("a")).unwrap();
        append(&path, &sample("b")).unwrap();
        let records = read(&path).unwrap();
        let names: Vec<_> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(records[0], sample("a").with_time(records[0].at_unix_ms));
    }

    #[test]
    fn compaction_bounds_the_file_to_the_newest_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RUN_LOG_FILE);
        for i in 0..50 {
            append_bounded(&path, &sample(&i.to_string()), 5, 2048).unwrap();
        }
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.len() < 2048 + 512,
            "compaction keeps the file small"
        );
        let records = read_bounded(&path, 5).unwrap();
        assert_eq!(records.len(), 5);
        assert_eq!(records.last().unwrap().name, "49");
    }

    #[test]
    fn read_caps_at_capacity_and_skips_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RUN_LOG_FILE);
        for i in 0..8 {
            append(&path, &sample(&i.to_string())).unwrap();
        }
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"not json\n").unwrap();
        let records = read_bounded(&path, 3).unwrap();
        let names: Vec<_> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["5", "6", "7"]);
    }

    impl RunRecord {
        fn with_time(mut self, at_unix_ms: u64) -> Self {
            self.at_unix_ms = at_unix_ms;
            self
        }
    }
}
