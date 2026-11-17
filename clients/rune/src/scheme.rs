//! Linux: the registered desktop entry opens `phux://` links with this app.
//! The installer registers it; an entry from before link support, which the
//! in-app updater leaves as it was, gains the handler at the next launch.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const ENTRY: &str = "sh.phux.rune.desktop";
const MIME: &str = "x-scheme-handler/phux";

/// Add the handler to the installed entry when it lacks one. Best effort:
/// blocks on the desktop's tools, so it runs on a thread of its own.
pub fn repair() {
    let Some(dir) = applications() else { return };
    let path = dir.join(ENTRY);
    let Some(entry) = std::fs::read_to_string(&path).ok() else {
        return;
    };
    let Some(repaired) = with_handler(&entry) else {
        return;
    };
    let staged = dir.join(format!(".{ENTRY}.phux-rune"));
    if std::fs::write(&staged, repaired).is_err() || std::fs::rename(&staged, &path).is_err() {
        let _ = std::fs::remove_file(&staged);
        return;
    }
    quietly("update-desktop-database", &[dir.as_os_str()]);
    quietly(
        "xdg-mime",
        &["default".as_ref(), ENTRY.as_ref(), MIME.as_ref()],
    );
}

fn applications() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(data.join("applications"))
}

fn quietly(tool: &str, args: &[&std::ffi::OsStr]) {
    let _ = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `entry` with `%u` on its Exec line and the `phux` scheme in its MimeType,
/// or `None` when it already names the scheme.
fn with_handler(entry: &str) -> Option<String> {
    if entry
        .lines()
        .any(|line| line.starts_with("MimeType=") && line.contains(MIME))
    {
        return None;
    }
    let mut mime = false;
    let mut lines: Vec<String> = entry
        .lines()
        .map(|line| {
            if line.starts_with("Exec=") && !line.contains("%u") && !line.contains("%U") {
                format!("{line} %u")
            } else if let Some(types) = line.strip_prefix("MimeType=") {
                mime = true;
                let sep = if types.is_empty() || types.ends_with(';') {
                    ""
                } else {
                    ";"
                };
                format!("MimeType={types}{sep}{MIME};")
            } else {
                line.to_owned()
            }
        })
        .collect();
    if !mime {
        lines.push(format!("MimeType={MIME};"));
    }
    Some(lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_entry_gains_the_handler_once() {
        let old = "[Desktop Entry]\nExec=\"/opt/phux-rune/phux-rune\"\nIcon=x\n";
        let new = with_handler(old).expect("repaired");
        assert_eq!(
            new,
            "[Desktop Entry]\nExec=\"/opt/phux-rune/phux-rune\" %u\nIcon=x\nMimeType=x-scheme-handler/phux;\n"
        );
        assert_eq!(with_handler(&new), None);
        let shipped = include_str!("../packaging/sh.phux.rune.desktop");
        assert_eq!(with_handler(shipped), None, "the shipped entry has it");
    }

    #[test]
    fn other_types_are_kept() {
        let entry = "Exec=phux-rune %U\nMimeType=text/plain";
        assert_eq!(
            with_handler(entry).as_deref(),
            Some("Exec=phux-rune %U\nMimeType=text/plain;x-scheme-handler/phux;\n")
        );
    }
}
