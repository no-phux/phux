//! Paths for a `[[startup]]` sidecar's log, pidfile, and private state
//! directory. The server writes them; `phux plugin log --startup` reads
//! the log.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Directory under the profile state dir that holds every sidecar.
pub const DIR_NAME: &str = "plugin-startup";

/// Rotate a sidecar log once it reaches this size.
pub const LOG_MAX_BYTES: u64 = 1024 * 1024;

/// Rotated generations kept (`<log>.1` .. `<log>.3`).
pub const LOG_GENERATIONS: usize = 3;

/// Mode of the per-sidecar state directory (`PHUX_PLUGIN_STATE`).
pub const STATE_DIR_MODE: u32 = 0o700;

/// Mode of the log and pidfile.
pub const LOG_FILE_MODE: u32 = 0o600;

/// One sidecar log on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupLog {
    /// Manifest plugin id.
    pub plugin_id: String,
    /// `[[startup]]` id.
    pub startup_id: String,
    /// Live log path (rotations are `<path>.1` and so on).
    pub path: PathBuf,
}

/// `<state>/plugin-startup/<plugin-id>/`.
#[must_use]
pub fn plugin_dir(state: &Path, plugin_id: &str) -> PathBuf {
    state.join(DIR_NAME).join(plugin_id)
}

/// `PHUX_PLUGIN_STATE`: `<state>/plugin-startup/<plugin-id>/<startup-id>/`.
#[must_use]
pub fn state_dir(state: &Path, plugin_id: &str, startup_id: &str) -> PathBuf {
    plugin_dir(state, plugin_id).join(startup_id)
}

/// `<state>/plugin-startup/<plugin-id>/<startup-id>.log`.
#[must_use]
pub fn log_path(state: &Path, plugin_id: &str, startup_id: &str) -> PathBuf {
    plugin_dir(state, plugin_id).join(format!("{startup_id}.log"))
}

/// `<state>/plugin-startup/<plugin-id>/<startup-id>.pid`.
#[must_use]
pub fn pid_path(state: &Path, plugin_id: &str, startup_id: &str) -> PathBuf {
    plugin_dir(state, plugin_id).join(format!("{startup_id}.pid"))
}

/// The `n`th rotated generation of `log`: `log.1`, `log.2`, …
#[must_use]
pub fn generation_path(log: &Path, n: usize) -> PathBuf {
    let mut path = log.as_os_str().to_owned();
    path.push(format!(".{n}"));
    PathBuf::from(path)
}

/// Every live sidecar log under `state`, in plugin-id then startup-id order.
/// A missing directory is an empty list.
///
/// # Errors
///
/// When a directory that exists cannot be read.
pub fn list_logs(state: &Path) -> io::Result<Vec<StartupLog>> {
    let root = state.join(DIR_NAME);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut logs = Vec::new();
    for plugin in entries {
        let plugin = plugin?;
        if !plugin.file_type()?.is_dir() {
            continue;
        }
        let plugin_id = plugin.file_name().to_string_lossy().into_owned();
        for file in fs::read_dir(plugin.path())? {
            let file = file?;
            let name = file.file_name();
            let Some(startup_id) = name.to_str().and_then(|n| n.strip_suffix(".log")) else {
                continue;
            };
            if startup_id.is_empty() || startup_id.contains('.') {
                continue;
            }
            logs.push(StartupLog {
                plugin_id: plugin_id.clone(),
                startup_id: startup_id.to_owned(),
                path: file.path(),
            });
        }
    }
    logs.sort_by(|a, b| (&a.plugin_id, &a.startup_id).cmp(&(&b.plugin_id, &b.startup_id)));
    Ok(logs)
}

/// The last `lines` of `path`, filling from the newest rotated generation
/// when the live file is shorter than that (a copy-then-truncate just
/// moved the history to `<path>.1`).
///
/// # Errors
///
/// When the live file exists but cannot be read. A missing live file with
/// no generation is an empty tail.
pub fn tail(path: &Path, lines: usize) -> io::Result<String> {
    let mut text = String::new();
    let previous = generation_path(path, 1);
    if previous.is_file() {
        text.push_str(&fs::read_to_string(&previous)?);
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
    }
    if path.is_file() {
        text.push_str(&fs::read_to_string(path)?);
    }
    if lines == 0 || text.is_empty() {
        return Ok(String::new());
    }
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    Ok(all[start..].join("\n"))
}
