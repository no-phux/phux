//! `phux agent doc folder PATH`: a directory as a Markdown document,
//! reread every few seconds and republished when it changed. A `navigate`
//! intent opens a subdirectory (`..` goes up), never above PATH; each file
//! names the `phux download` that fetches it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use phux_client::tree::DocumentInput;

use super::doc_producer::Producer;
use super::report;
use crate::commands::cli_runtime;
use crate::exit_codes::EXIT_FAILURE;

/// The most entries one document lists.
const MAX_ENTRIES: usize = 500;

/// One directory entry as the listing shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    /// `None` for a directory.
    size: Option<u64>,
    modified: Option<SystemTime>,
}

pub(super) fn run(
    path: &Path,
    name: Option<&str>,
    pane: Option<&str>,
    interval: Option<u64>,
    socket: Option<PathBuf>,
) -> ExitCode {
    let root = match std::fs::canonicalize(path) {
        Ok(root) if root.is_dir() => root,
        Ok(_) => return refuse(&format!("{} is not a directory", path.display())),
        Err(err) => return refuse(&format!("{}: {err}", path.display())),
    };
    let label = root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let name = name.unwrap_or(&label).to_owned();
    // ponytail: polls; inotify/FSEvents only if a 2 s lag matters.
    let interval = Duration::from_secs(interval.unwrap_or(2).max(1));
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let verb = "agent doc folder";
        let opened = Producer::open(socket, pane, "text/markdown", Some(&name), verb).await;
        let mut producer = match opened {
            Ok(producer) => producer,
            Err(code) => return code,
        };
        let mut dir = root.clone();
        let mut shown = None;
        let mut tick = tokio::time::interval(interval);
        loop {
            let doc = render(&root, &dir, &list(&dir));
            if shown.as_ref() != Some(&doc) {
                if let Err(err) = producer.publish(doc.clone().into_bytes()).await {
                    return report(false, &err);
                }
                shown = Some(doc);
            }
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
                _ = tick.tick() => {}
                woke = producer.recv() => match producer.settle(woke).await {
                    Ok(Some(DocumentInput::Navigate { url })) => {
                        if let Some(next) = navigate(&root, &dir, &url) {
                            dir = next;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => return report(false, &err),
                },
            }
        }
    })
}

fn refuse(why: &str) -> ExitCode {
    eprintln!("phux agent doc folder: {why}");
    ExitCode::from(EXIT_FAILURE)
}

/// `dir`'s entries, directories first, then by name; empty when it cannot
/// be read (the listing says so by being empty).
fn list(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                size: (!meta.is_dir()).then_some(meta.len()),
                modified: meta.modified().ok(),
            })
        })
        .collect();
    entries.sort_by(|a, b| (a.size.is_some(), &a.name).cmp(&(b.size.is_some(), &b.name)));
    entries
}

/// Where `url` leads from `dir`: a subdirectory, `..`, or a path under
/// `root`; `None` for anything outside `root` or not a directory.
fn navigate(root: &Path, dir: &Path, url: &str) -> Option<PathBuf> {
    let next = std::fs::canonicalize(dir.join(url.trim())).ok()?;
    (next.starts_with(root) && next.is_dir()).then_some(next)
}

/// The Markdown listing of `dir` (under `root`).
fn render(root: &Path, dir: &Path, entries: &[Entry]) -> String {
    use std::fmt::Write as _;

    let mut doc = format!("# {}\n\n`{}`", title(root, dir), dir.display());
    let _ = write!(doc, " · {} entries\n\n", entries.len());
    if entries.is_empty() {
        doc.push_str("_Empty, or unreadable._\n");
        return doc;
    }
    doc.push_str("| Name | Size | Modified |\n|---|---:|---|\n");
    for entry in entries.iter().take(MAX_ENTRIES) {
        let (name, size) = entry.size.map_or_else(
            || (format!("{}/", entry.name), String::new()),
            |bytes| (entry.name.clone(), human(bytes)),
        );
        let when = entry.modified.map_or_else(String::new, |at| {
            chrono::DateTime::<chrono::Local>::from(at)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        });
        let _ = writeln!(doc, "| {} | {size} | {when} |", name.replace('|', "\\|"));
    }
    if entries.len() > MAX_ENTRIES {
        let _ = writeln!(doc, "\n…and {} more.", entries.len() - MAX_ENTRIES);
    }
    let _ = write!(
        doc,
        "\nFetch a file: `phux download HOST:{}/NAME`. Open a folder: \
         navigate to its name, or `..`.\n",
        dir.display()
    );
    doc
}

/// `root`'s name, then the path below it.
fn title(root: &Path, dir: &Path) -> String {
    let base = root
        .file_name()
        .map_or_else(|| "/".to_owned(), |n| n.to_string_lossy().into_owned());
    match dir.strip_prefix(root) {
        Ok(rest) if !rest.as_os_str().is_empty() => format!("{base}/{}", rest.display()),
        _ => base,
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    #[allow(clippy::cast_precision_loss, reason = "a size label")]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_shows_folders_first_and_how_to_fetch() {
        let root = Path::new("/srv/files");
        let entries = [
            Entry {
                name: "photos".into(),
                size: None,
                modified: None,
            },
            Entry {
                name: "a|b.txt".into(),
                size: Some(2048),
                modified: None,
            },
        ];
        let doc = render(root, &root.join("docs"), &entries);
        assert!(doc.starts_with("# files/docs\n\n`/srv/files/docs` · 2 entries"));
        assert!(doc.contains("| photos/ |  |  |"));
        assert!(doc.contains("| a\\|b.txt | 2.0 KB |  |"));
        assert!(doc.contains("`phux download HOST:/srv/files/docs/NAME`"));
        assert!(render(root, root, &[]).contains("_Empty, or unreadable._"));
        assert_eq!(human(10), "10 B");
    }

    #[test]
    fn navigation_stays_under_the_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canon");
        std::fs::create_dir(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("file"), b"x").expect("write");
        let sub = navigate(&root, &root, "sub").expect("into sub");
        assert_eq!(sub, root.join("sub"));
        assert_eq!(navigate(&root, &sub, "..").as_deref(), Some(root.as_path()));
        assert!(navigate(&root, &root, "..").is_none(), "not above the root");
        assert!(navigate(&root, &root, "file").is_none(), "not a file");
        let names: Vec<_> = list(&root).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["sub", "file"]);
    }
}
