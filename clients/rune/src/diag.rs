//! What this app leaves behind when something goes wrong, on this computer
//! only: nothing here touches the network. A panic gets an entry of its own,
//! notable events go to a small log, and **Copy diagnostics** in Settings puts
//! both on the clipboard for the person to read and share by hand. Every line
//! is redacted before it reaches a file or the clipboard.

// The GPUI desktop's redaction, verbatim and with its tests: it depends on
// nothing, while the rest of that diag module is tied to GPUI.
#[path = "../../desktop/native/src/diag/redact.rs"]
mod redact;

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::updates::VERSION;

/// Events: startup and every update outcome.
pub const EVENTS: &str = "rune.log";
pub const PANICS: &str = "rune-panic.log";
/// Also the stdout of the macOS update helper, which outlives the app.
pub const UPDATES: &str = "rune-update.log";
/// A log rolls to `<name>.1` here, so it can't fill a disk.
const MAX_LOG: u64 = 1 << 20;
/// How much of each log a report carries.
const REPORT_TAIL: usize = 16 * 1024;

/// The directory every log lives in: what macOS's Console shows, or the XDG
/// state directory. None in tests, which must not write to the developer's
/// logs: Copy diagnostics would carry their lines.
pub fn dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    Some(dir_under(
        Path::new(&home),
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
    ))
}

fn dir_under(home: &Path, state: Option<PathBuf>) -> PathBuf {
    if cfg!(target_os = "macos") {
        return home.join("Library/Logs/Phux");
    }
    state
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"))
        .join("phux-rune")
}

/// Credentials and the home directory out of `text`.
fn clean(text: &str) -> String {
    let text = redact::redact(text);
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => text.replace(&home, "~"),
        _ => text,
    }
}

fn stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn append_in(dir: &Path, name: &str, text: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_LOG) {
        fs::rename(&path, dir.join(format!("{name}.1")))?;
    }
    let line = format!("{} {VERSION} {}\n", stamp(), clean(text));
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

/// Add a line to the log called `name`. Losing one never stops the app.
pub fn note(name: &str, text: &str) {
    if let Some(dir) = dir() {
        let _ = append_in(&dir, name, text);
    }
}

fn panic_text(thread: Option<&str>, info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-text panic");
    let location = info
        .location()
        .map_or_else(String::new, |at| format!(" at {at}"));
    let thread = thread.unwrap_or("unnamed");
    let trace = std::backtrace::Backtrace::force_capture();
    format!("panic on {thread}{location}: {message}\n{trace}")
}

/// Record every panic, then let the default hook print it as usual.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        note(PANICS, &panic_text(thread.name(), info));
        previous(info);
    }));
    note(
        EVENTS,
        &format!(
            "started on {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
}

fn tail(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(REPORT_TAIL);
    Some(String::from_utf8_lossy(&bytes[start..]).into_owned())
}

fn report_from(dir: Option<&Path>, facts: &[(&str, String)]) -> String {
    let mut report = format!(
        "Phux Rune {VERSION} on {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    for (name, value) in facts {
        report.push_str(&format!("{name}: {value}\n"));
    }
    for name in [PANICS, EVENTS, UPDATES] {
        let body = dir.and_then(|dir| tail(&dir.join(name)));
        let body = body.as_deref().unwrap_or("(empty)\n");
        report.push_str(&format!("\n--- {name}\n{body}"));
    }
    clean(&report)
}

/// `facts` and the tail of each log, for the clipboard.
pub fn report(facts: &[(&str, String)]) -> String {
    report_from(dir().as_deref(), facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phux-rune-diag-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn logs_go_where_each_platform_keeps_them() {
        let home = Path::new("/home/me");
        if cfg!(target_os = "macos") {
            assert_eq!(dir_under(home, None), home.join("Library/Logs/Phux"));
            return;
        }
        assert_eq!(dir_under(home, None), home.join(".local/state/phux-rune"));
        assert_eq!(
            dir_under(home, Some("/state".into())),
            PathBuf::from("/state/phux-rune")
        );
        assert_eq!(
            dir_under(home, Some("relative".into())),
            home.join(".local/state/phux-rune")
        );
    }

    #[test]
    fn a_line_is_redacted_before_it_reaches_the_file() {
        let dir = temp("redact");
        append_in(&dir, EVENTS, "joined with token=abc123").unwrap();
        let written = fs::read_to_string(dir.join(EVENTS)).unwrap();
        assert!(!written.contains("abc123"), "{written}");
        assert!(written.contains(VERSION));
    }

    #[test]
    fn a_long_log_rolls_over() {
        let dir = temp("roll");
        fs::create_dir_all(&dir).unwrap();
        let full = vec![b'x'; usize::try_from(MAX_LOG).unwrap() + 1];
        fs::write(dir.join(EVENTS), full).unwrap();
        append_in(&dir, EVENTS, "fresh").unwrap();
        assert!(fs::metadata(dir.join(format!("{EVENTS}.1"))).is_ok());
        assert!(fs::metadata(dir.join(EVENTS)).unwrap().len() < 200);
    }

    #[test]
    fn a_report_carries_facts_and_tails_and_no_secrets() {
        let dir = temp("report");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PANICS), "panic on main: password=hunter2\n").unwrap();
        fs::write(dir.join(EVENTS), vec![b'y'; REPORT_TAIL * 2]).unwrap();
        let report = report_from(Some(&dir), &[("status", "Attached".to_owned())]);
        assert!(report.contains("status: Attached"));
        assert!(report.contains("--- rune-panic.log\npanic on main"));
        assert!(!report.contains("hunter2"), "{report}");
        assert!(report.contains("--- rune-update.log\n(empty)"));
        assert!(report.len() < REPORT_TAIL * 2);
    }

    #[test]
    fn a_report_survives_no_log_directory() {
        assert!(report_from(None, &[]).contains("--- rune.log\n(empty)"));
    }

    #[test]
    fn a_forced_panic_is_formatted_with_its_place_and_message() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = captured.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            *sink.lock().unwrap() = panic_text(Some("worker"), info);
        }));
        let _ = std::panic::catch_unwind(|| panic!("forced: {}", 7));
        std::panic::set_hook(previous);
        let text = captured.lock().unwrap().clone();
        assert!(text.starts_with("panic on worker at "), "{text}");
        assert!(text.contains("diag.rs"), "{text}");
        assert!(text.contains("forced: 7"), "{text}");
    }
}
