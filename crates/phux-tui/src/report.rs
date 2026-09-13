//! Local bug-report bundles correlated with the session and logs.
//!
//! The TUI `report-bug` action (and `phux report new`) writes one directory
//! under [`phux_config::instance::reports_dir`]: an id, a human `report.md`
//! an agent can open, structured `meta.json`, log tails, and an optional
//! screen dump. `latest` points at the newest bundle so a new session can
//! find it without copying a long path.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Leaf name of the pointer at the newest report.
pub const LATEST_NAME: &str = "latest";

/// Default number of trailing log lines captured into a bundle.
pub const LOG_TAIL_LINES: usize = 400;

/// Inputs for one bundle. Missing fields stay omitted from the markdown;
/// logs are always tailed from the profile state dir when those files exist.
#[derive(Debug, Clone, Default)]
pub struct ReportDraft {
    /// Optional free-text note from the user.
    pub note: Option<String>,
    /// Attached session name.
    pub session: Option<String>,
    /// Focused pane selector (`@N` or `host/@N`).
    pub pane: Option<String>,
    /// Active window index.
    pub window: Option<usize>,
    /// Outer viewport in cells.
    pub viewport: Option<(u16, u16)>,
    /// Whether the focused pane is on the alternate screen.
    pub alt_screen: Option<bool>,
    /// Whether the focused pane's app enabled mouse tracking.
    pub mouse_tracking: Option<bool>,
    /// Plain-text dump of the focused pane, when one could be read.
    pub screen: Option<String>,
    /// Binary version string.
    pub version: String,
}

/// A bundle that landed on disk.
#[derive(Debug, Clone)]
pub struct WrittenReport {
    /// Directory name and the id printed to the user.
    pub id: String,
    /// Absolute path of that directory.
    pub dir: PathBuf,
}

/// Profile-scoped reports directory.
#[must_use]
pub fn reports_dir() -> PathBuf {
    phux_config::instance::reports_dir()
}

/// Path of the newest-report pointer inside `dir`.
#[must_use]
pub fn latest_pointer(dir: &Path) -> PathBuf {
    dir.join(LATEST_NAME)
}

/// Write `draft` under the profile reports directory.
///
/// # Errors
///
/// Returns if the directory cannot be created or a file cannot be written.
pub fn write_bundle(draft: &ReportDraft) -> io::Result<WrittenReport> {
    write_bundle_in(&reports_dir(), draft)
}

/// Write `draft` under `root` (the reports directory). Test seam for
/// [`write_bundle`].
///
/// # Errors
///
/// Returns if the directory cannot be created or a file cannot be written.
pub fn write_bundle_in(root: &Path, draft: &ReportDraft) -> io::Result<WrittenReport> {
    fs::create_dir_all(root)?;
    let id = new_report_id();
    let dir = root.join(&id);
    fs::create_dir_all(&dir)?;

    let captured_at = iso8601_now();
    let state = phux_config::instance::state_dir();
    let server_log = state.join("server.log");
    let client_log = state.join(format!("client-{}.log", std::process::id()));
    let server_tail = tail_file(&server_log, LOG_TAIL_LINES);
    let client_tail = tail_file(&client_log, LOG_TAIL_LINES);

    let meta = Meta {
        schema_version: 1,
        id: id.clone(),
        captured_at: captured_at.clone(),
        version: draft.version.clone(),
        profile: phux_config::instance::profile(),
        pid: std::process::id(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        session: draft.session.clone(),
        pane: draft.pane.clone(),
        window: draft.window,
        viewport: draft
            .viewport
            .map(|(cols, rows)| ViewportCells { cols, rows }),
        alt_screen: draft.alt_screen,
        mouse_tracking: draft.mouse_tracking,
        note: draft.note.clone(),
        server_log: server_log.display().to_string(),
        client_log: client_log.display().to_string(),
    };

    write_owner_only(
        &dir.join("meta.json"),
        &serde_json::to_vec_pretty(&meta)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
    )?;
    write_owner_only(
        &dir.join("report.md"),
        render_markdown(&meta, draft, root).as_bytes(),
    )?;
    if let Some(ref tail) = server_tail {
        write_owner_only(&dir.join("server.log.tail"), tail.as_bytes())?;
    }
    if let Some(ref tail) = client_tail {
        write_owner_only(&dir.join("client.log.tail"), tail.as_bytes())?;
    }
    if let Some(ref screen) = draft.screen {
        write_owner_only(&dir.join("screen.txt"), screen.as_bytes())?;
    }
    point_latest(root, &id)?;

    tracing::info!(id = %id, path = %dir.display(), "wrote bug report");
    Ok(WrittenReport { id, dir })
}

/// Reports currently on disk under `root`, newest first.
#[must_use]
pub fn list_reports(root: &Path) -> Vec<WrittenReport> {
    let mut reports = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && entry.file_name() != LATEST_NAME
            })
            .map(|entry| WrittenReport {
                id: entry.file_name().to_string_lossy().into_owned(),
                dir: entry.path(),
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    reports.sort_by(|a, b| b.id.cmp(&a.id));
    reports
}

/// Resolve `id` (or the latest pointer when `id` is `None`) to a bundle.
#[must_use]
pub fn resolve_report(root: &Path, id: Option<&str>) -> Option<WrittenReport> {
    match id {
        Some(id) if id != LATEST_NAME => {
            let dir = root.join(id);
            dir.is_dir().then(|| WrittenReport {
                id: id.to_owned(),
                dir,
            })
        }
        _ => latest_report(root),
    }
}

/// The bundle `latest` currently names, if any.
#[must_use]
pub fn latest_report(root: &Path) -> Option<WrittenReport> {
    let pointer = latest_pointer(root);
    let target = fs::read_link(&pointer)
        .ok()
        .or_else(|| fs::read_to_string(&pointer).ok().map(PathBuf::from))?;
    let dir = if target.is_absolute() {
        target
    } else {
        root.join(target)
    };
    let id = dir.file_name()?.to_string_lossy().into_owned();
    dir.is_dir().then(|| WrittenReport { id, dir })
}

#[derive(Debug, Serialize)]
struct Meta {
    schema_version: u32,
    id: String,
    captured_at: String,
    version: String,
    profile: String,
    pid: u32,
    os: &'static str,
    arch: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    viewport: Option<ViewportCells>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alt_screen: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mouse_tracking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    server_log: String,
    client_log: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct ViewportCells {
    cols: u16,
    rows: u16,
}

fn new_report_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("r-{}-{:04x}", now.as_secs(), now.subsec_micros() & 0xffff)
}

fn iso8601_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}Z", now.as_secs(), now.subsec_millis())
}

fn render_markdown(meta: &Meta, draft: &ReportDraft, root: &Path) -> String {
    let mut body = String::new();
    body.push_str(&format!("# phux bug report `{}`\n\n", meta.id));
    body.push_str("Hand this path to an agent:\n\n");
    body.push_str(&format!("    {}\n\n", root.join(&meta.id).display()));
    body.push_str("Or from a shell:\n\n");
    body.push_str(&format!("    phux report show {}\n\n", meta.id));
    body.push_str(&format!("- captured: {}\n", meta.captured_at));
    body.push_str(&format!("- version: {}\n", meta.version));
    body.push_str(&format!("- profile: {}\n", meta.profile));
    body.push_str(&format!("- pid: {}\n", meta.pid));
    body.push_str(&format!("- os: {}/{}\n", meta.os, meta.arch));
    if let Some(ref session) = meta.session {
        body.push_str(&format!("- session: {session}\n"));
    }
    if let Some(ref pane) = meta.pane {
        body.push_str(&format!("- pane: {pane}\n"));
    }
    if let Some(window) = meta.window {
        body.push_str(&format!("- window: {window}\n"));
    }
    if let Some(vp) = meta.viewport {
        body.push_str(&format!("- viewport: {}x{}\n", vp.cols, vp.rows));
    }
    if let Some(alt) = meta.alt_screen {
        body.push_str(&format!("- alt_screen: {alt}\n"));
    }
    if let Some(mouse) = meta.mouse_tracking {
        body.push_str(&format!("- mouse_tracking: {mouse}\n"));
    }
    if let Some(ref note) = meta.note {
        body.push_str("\n## Note\n\n");
        body.push_str(note);
        body.push_str("\n");
    }
    body.push_str("\n## Files in this bundle\n\n");
    body.push_str("- `report.md` — this file\n");
    body.push_str("- `meta.json` — the same fields as structured data\n");
    if draft.screen.is_some() {
        body.push_str("- `screen.txt` — focused pane dump at capture time\n");
    }
    body.push_str("- `server.log.tail` / `client.log.tail` — last log lines, when present\n");
    body.push_str(&format!("- server log path: {}\n", meta.server_log));
    body.push_str(&format!("- client log path: {}\n", meta.client_log));
    if let Some(ref screen) = draft.screen {
        body.push_str("\n## Screen\n\n```\n");
        body.push_str(screen);
        if !screen.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("```\n");
    }
    body
}

fn tail_file(path: &Path, lines: usize) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    if text.is_empty() {
        return None;
    }
    let collected: Vec<&str> = text.lines().rev().take(lines).collect();
    let mut out = String::new();
    for line in collected.into_iter().rev() {
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

fn point_latest(root: &Path, id: &str) -> io::Result<()> {
    let pointer = latest_pointer(root);
    let _ = fs::remove_file(&pointer);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(id, &pointer)?;
    }
    #[cfg(not(unix))]
    {
        write_owner_only(&pointer, id.as_bytes())?;
    }
    Ok(())
}

fn write_owner_only(path: &Path, contents: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        let mut perms = file.metadata()?.permissions();
        perms.set_mode(0o600);
        file.set_permissions(perms)?;
        file.write_all(contents)?;
        file.flush()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn draft(note: &str) -> ReportDraft {
        ReportDraft {
            note: Some(note.to_owned()),
            session: Some("work".to_owned()),
            pane: Some("@7".to_owned()),
            window: Some(0),
            viewport: Some((80, 24)),
            alt_screen: Some(true),
            mouse_tracking: Some(true),
            screen: Some("hello from grok\n".to_owned()),
            version: "0.0-test".to_owned(),
        }
    }

    #[test]
    fn write_bundle_in_creates_markdown_and_latest_pointer() {
        let tmp = TempDir::new().expect("tmp");
        let written = write_bundle_in(tmp.path(), &draft("scrolling is wrong")).expect("write");
        assert!(written.dir.join("report.md").is_file());
        assert!(written.dir.join("meta.json").is_file());
        assert!(written.dir.join("screen.txt").is_file());
        let markdown = fs::read_to_string(written.dir.join("report.md")).expect("md");
        assert!(markdown.contains("scrolling is wrong"));
        assert!(markdown.contains("phux report show"));
        assert!(markdown.contains("@7"));
        let latest = latest_report(tmp.path()).expect("latest");
        assert_eq!(latest.id, written.id);
        let listed = list_reports(tmp.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, written.id);
    }

    #[test]
    fn resolve_report_accepts_latest_and_id() {
        let tmp = TempDir::new().expect("tmp");
        let first = write_bundle_in(tmp.path(), &draft("one")).expect("first");
        let second = write_bundle_in(tmp.path(), &draft("two")).expect("second");
        assert_eq!(
            resolve_report(tmp.path(), None).expect("latest").id,
            second.id
        );
        assert_eq!(
            resolve_report(tmp.path(), Some(&first.id))
                .expect("first")
                .id,
            first.id
        );
        assert!(resolve_report(tmp.path(), Some("missing")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn bundle_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().expect("tmp");
        let written = write_bundle_in(tmp.path(), &draft("perms")).expect("write");
        let mode = fs::metadata(written.dir.join("report.md"))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
