//! Size-based rotation of the canonical server log while the server runs
//! (see [`super`]).

use std::path::{Path, PathBuf};

use super::{harden_log_sink, server_log_path};

/// Size at which a log is rolled aside, bounding growth within one run and
/// across many short-lived servers.
pub(super) const LOG_ROTATE_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;

/// Rotated generations kept (`<path>.1` ..), capping total history. A
/// constant because the config schema is frozen (ADR-0071).
pub(super) const LOG_ROTATE_MAX_GENERATIONS: usize = 4;

/// How often [`run_log_rotation_task`] checks the server log's size.
pub(super) const LOG_ROTATE_CHECK_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(300);

/// The path of the `n`th rotated generation of `base`: `base.1`, `base.2`, …
pub(super) fn generation_path(base: &Path, n: usize) -> PathBuf {
    let mut path = base.as_os_str().to_owned();
    path.push(format!(".{n}"));
    PathBuf::from(path)
}

/// Renames that shift rotated generations up one slot, highest first so
/// nothing is clobbered early. The last rename overwrites `.{max}`, which
/// is how the oldest generation is dropped. Empty for `max_generations <= 1`.
pub(super) fn shift_plan(base: &Path, max_generations: usize) -> Vec<(PathBuf, PathBuf)> {
    (1..max_generations)
        .rev()
        .map(|n| (generation_path(base, n), generation_path(base, n + 1)))
        .collect()
}

/// Roll `path` aside past `threshold`, keeping up to `max_generations`
/// generations: shift, copy the live file to `.1`, then truncate `path` in
/// place. Truncating (not renaming) keeps existing writers and `tail -f`
/// readers on the same inode. Returns whether it rotated; callers ignore
/// errors, since logging must never stop the server.
pub(super) fn rotate_log(
    path: &Path,
    threshold: u64,
    max_generations: usize,
) -> std::io::Result<bool> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(false);
    };
    if meta.len() < threshold {
        return Ok(false);
    }
    for (from, to) in shift_plan(path, max_generations) {
        if from.exists() {
            std::fs::rename(&from, &to)?;
        }
    }
    if max_generations > 0 {
        let gen1 = generation_path(path, 1);
        std::fs::copy(path, &gen1)?;
        // Re-harden explicitly (ADR-0028) rather than rely on `fs::copy`.
        harden_log_sink(&gen1)?;
    }
    // In place, not rename + recreate (see above).
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    Ok(true)
}

/// One rotation check of the server log at `path` with production limits
/// (the synchronous body of each [`run_log_rotation_task`] tick).
pub(super) fn rotate_server_log_if_needed(path: &Path) -> std::io::Result<bool> {
    rotate_log(path, LOG_ROTATE_THRESHOLD_BYTES, LOG_ROTATE_MAX_GENERATIONS)
}

/// Periodically rotate the canonical `server.log` while the server runs; spawn
/// once on the server runtime.
///
/// The first tick is immediate. Each check runs in `spawn_blocking`, since a
/// rotation copies megabytes and must not stall the current-thread reactor
/// (ADR-0003).
pub async fn run_log_rotation_task() {
    let mut ticker = tokio::time::interval(LOG_ROTATE_CHECK_INTERVAL);
    loop {
        ticker.tick().await;
        let outcome =
            tokio::task::spawn_blocking(|| rotate_server_log_if_needed(&server_log_path())).await;
        if let Ok(Err(err)) = outcome {
            tracing::debug!(error = %err, "server log rotation check failed");
        }
        // A panicked or failed check is ignored.
    }
}
