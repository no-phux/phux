//! What survives between collector runs.
//!
//! Each network source's last good plan, its last attempt, and its
//! rate-limit backoff. Every hook run is a
//! fresh process, so without this a busy host would poll a vendor on every
//! agent state change.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Plan;

/// Never ask one vendor more often than this, absent a refresh request.
pub const MIN_INTERVAL_MS: i64 = 120_000;
/// A reading older than this is reported `stale`.
pub const STALE_AFTER_MS: i64 = 30 * 60_000;
/// First 429 backoff; doubles per consecutive 429.
const BACKOFF_BASE_MS: i64 = 3 * 60_000;
/// Backoff ceiling.
const BACKOFF_CAP_MS: i64 = 15 * 60_000;

/// The 429 ladder: 3, 6, 12, then 15 minutes.
#[must_use]
pub fn backoff_ms(consecutive_429s: u32) -> i64 {
    if consecutive_429s == 0 {
        return 0;
    }
    let shift = (consecutive_429s - 1).min(8);
    (BACKOFF_BASE_MS << shift).min(BACKOFF_CAP_MS)
}

/// Persisted collector state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Per network source (`claude`, `codex-wham`, `grok`).
    #[serde(default)]
    pub sources: BTreeMap<String, SourceState>,
    /// The newest `phux.usage.refresh/v1` value already honored.
    #[serde(default)]
    pub refresh_seen: Option<String>,
}

/// One network source's history.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceState {
    /// The last plan read successfully.
    #[serde(default)]
    pub last_good: Option<Plan>,
    /// When the last request was made (Unix ms).
    #[serde(default)]
    pub last_attempt_ms: i64,
    /// Consecutive 429 answers.
    #[serde(default)]
    pub rate_limited: u32,
    /// No request before this (Unix ms).
    #[serde(default)]
    pub retry_at_ms: i64,
    /// Why the last request failed, until one succeeds.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Whether that failure was the vendor refusing the credential.
    #[serde(default)]
    pub rejected: bool,
}

impl SourceState {
    /// Whether a request may go out now. `eager` (a manual or client
    /// refresh) skips the minimum interval but never a 429 backoff.
    #[must_use]
    pub const fn due(&self, now_ms: i64, eager: bool) -> bool {
        if now_ms < self.retry_at_ms {
            return false;
        }
        eager
            || self.last_attempt_ms == 0
            || now_ms >= self.last_attempt_ms.saturating_add(MIN_INTERVAL_MS)
    }

    /// Record a request about to go out.
    pub const fn attempted(&mut self, now_ms: i64) {
        self.last_attempt_ms = now_ms;
    }

    /// Record a good reading.
    pub fn succeeded(&mut self, plan: Plan) {
        self.last_good = Some(plan);
        self.rate_limited = 0;
        self.retry_at_ms = 0;
        self.last_error = None;
        self.rejected = false;
    }

    /// Record a failed request.
    pub fn failed(&mut self, why: String, rejected: bool) {
        self.last_error = Some(why);
        self.rejected = rejected;
    }

    /// Record a 429 and return how long to wait.
    pub fn throttled(&mut self, now_ms: i64) -> i64 {
        self.rate_limited = self.rate_limited.saturating_add(1);
        let wait = backoff_ms(self.rate_limited);
        self.retry_at_ms = now_ms + wait;
        wait
    }
}

/// The state file, held under an exclusive lock for the whole run so two
/// hooks firing together make one request, not two.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    _lock: File,
}

impl Store {
    /// Lock and open `dir/state.json`, creating `dir` (mode 0700 on Unix).
    ///
    /// # Errors
    ///
    /// When the directory or lock file cannot be created or locked.
    pub fn open(dir: &Path) -> std::io::Result<(Self, State)> {
        create_private_dir(dir)?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        lock.lock()?;
        let path = dir.join("state.json");
        let state = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok((Self { path, _lock: lock }, state))
    }

    /// Replace the state file atomically.
    ///
    /// # Errors
    ///
    /// When the file cannot be written or renamed.
    pub fn save(&self, state: &State) -> std::io::Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &self.path)
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_ladder_is_3_6_12_then_15_minutes() {
        let minutes: Vec<i64> = (0..6).map(|n| backoff_ms(n) / 60_000).collect();
        assert_eq!(minutes, [0, 3, 6, 12, 15, 15]);
        assert_eq!(backoff_ms(u32::MAX), BACKOFF_CAP_MS);
    }

    #[test]
    fn throttling_blocks_even_eager_requests_until_retry_at() {
        let mut source = SourceState::default();
        assert!(source.due(0, false));
        source.attempted(1_000);
        assert!(!source.due(2_000, false), "inside the minimum interval");
        assert!(source.due(2_000, true), "a refresh skips the interval");
        assert!(source.due(1_000 + MIN_INTERVAL_MS, false));

        let wait = source.throttled(10_000);
        assert_eq!(wait, 180_000);
        assert!(!source.due(10_000 + 179_999, true), "never skips a backoff");
        assert!(source.due(10_000 + 180_000, true));
        assert_eq!(source.throttled(200_000), 360_000);

        source.succeeded(crate::test_plan());
        assert_eq!((source.rate_limited, source.retry_at_ms), (0, 0));
    }

    #[test]
    fn state_round_trips_and_a_corrupt_file_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let (store, mut state) = Store::open(dir.path()).unwrap();
        assert_eq!(state, State::default());
        state
            .sources
            .entry("claude".into())
            .or_default()
            .throttled(5);
        state.refresh_seen = Some("42".into());
        store.save(&state).unwrap();
        drop(store);
        let (store, reread) = Store::open(dir.path()).unwrap();
        assert_eq!(reread, state);
        drop(store);
        std::fs::write(dir.path().join("state.json"), b"{not json").unwrap();
        let (_store, fresh) = Store::open(dir.path()).unwrap();
        assert_eq!(fresh, State::default());
    }
}
