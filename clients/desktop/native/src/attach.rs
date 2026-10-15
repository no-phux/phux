//! Attachments: dropped files and clipboard content become text a shell or a
//! coding agent reads. Files paste as shell-quoted paths; a clipboard image
//! (a screenshot, a copied picture) is written to a file whose path is pasted,
//! which agents such as Claude Code and Codex read as an image attachment.
//!
//! Those paths exist on this Mac. A terminal on another host does not receive
//! them: [`ControlPlane::put_file`](phux_client_runtime::control::ControlPlane::put_file)
//! can upload, but this client does not yet drain upload receipts, so a remote
//! terminal is refused instead of being handed a local path.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat};
use phux_protocol::ResourceId;

/// Where a terminal's files live.
///
/// `window_remote` on [`terminal_host`] is the registry host this window
/// attached to. Absent means the local server. A [`ResourceId::Local`] id on
/// that remote server is still remote: the id is local to the server, not to
/// this Mac. Readiness (`Connection::ready`, `input_ready`) is not an input
/// and does not make a host local.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalHost {
    /// This machine. Local paths may be pasted.
    Local,
    /// Another machine. `label` is a host name when one is known.
    Remote {
        /// Registry name, or the satellite host token.
        label: Option<String>,
    },
}

/// A path paste the user can see, or a refusal to show instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathAttach {
    /// Shell-quoted text to paste.
    Paste(String),
    /// Do not paste. Show this notice.
    Refuse {
        /// Notification title.
        title: &'static str,
        /// Notification body.
        body: String,
    },
}

/// Title for a local path that was not pasted into a remote terminal.
pub const PASTE_REFUSED: &str = "File not pasted";

/// A terminal is local only when this window's server is local and the
/// terminal is not a satellite.
#[must_use]
pub fn terminal_host(window_remote: Option<&str>, terminal: &ResourceId) -> TerminalHost {
    if let Some(host) = terminal.host() {
        return TerminalHost::Remote {
            label: Some(host.as_str().to_owned()),
        };
    }
    match window_remote.map(str::trim).filter(|name| !name.is_empty()) {
        Some(label) => TerminalHost::Remote {
            label: Some(label.to_owned()),
        },
        None => TerminalHost::Local,
    }
}

/// Quoted paths for a local terminal. A remote terminal yields a notice and
/// no paste text. Empty `paths` is nothing to do.
#[must_use]
pub fn attach_paths(host: &TerminalHost, paths: &[PathBuf]) -> Option<PathAttach> {
    if paths.is_empty() {
        return None;
    }
    match host {
        TerminalHost::Local => Some(PathAttach::Paste(quote_paths(paths))),
        TerminalHost::Remote { label } => Some(PathAttach::Refuse {
            title: PASTE_REFUSED,
            body: remote_paste_body(label.as_deref()),
        }),
    }
}

fn remote_paste_body(label: Option<&str>) -> String {
    let where_ = match label {
        Some(name) => format!("{name} is another machine"),
        None => "This terminal is on another machine".to_owned(),
    };
    format!(
        "{where_}. Nothing was pasted, because a path from this Mac would name the wrong file there."
    )
}

/// Shell-quote paths, one shell word each. A drop is text, never a command.
pub fn quote_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            let text = path.display().to_string();
            if text.chars().all(|ch| {
                ch.is_ascii_alphanumeric()
                    || matches!(
                        ch,
                        '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                    )
            }) {
                text
            } else {
                format!("'{}'", text.replace('\'', r#"'"'"'"#))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What Paste sends for this clipboard item on a local terminal: copied files
/// as quoted paths, else the text, else an image saved under [`dir`] as its
/// quoted path. `None` when there is nothing to paste or the image could not
/// be written.
///
/// Remote terminals go through [`clipboard_for_host`], which refuses file and
/// image paths instead of quoting them.
pub fn clipboard_paste(item: &ClipboardItem) -> Option<String> {
    match clipboard_for_host(item, &TerminalHost::Local)? {
        PathAttach::Paste(text) => Some(text),
        PathAttach::Refuse { .. } => None,
    }
}

/// Clipboard text is host-independent. Copied files and images are paths on
/// this Mac, so a remote terminal is refused and the image is not written.
#[must_use]
pub fn clipboard_for_host(item: &ClipboardItem, host: &TerminalHost) -> Option<PathAttach> {
    let entries = item.entries();
    if let Some(paths) = entries.iter().find_map(|entry| match entry {
        ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => Some(paths.paths()),
        _ => None,
    }) {
        return attach_paths(host, paths);
    }
    if let Some(text) = item.text().filter(|text| !text.is_empty()) {
        return Some(PathAttach::Paste(text));
    }
    let image = entries.iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(image),
        _ => None,
    })?;
    match host {
        TerminalHost::Local => match save_image(image, &dir()) {
            Ok(path) => Some(PathAttach::Paste(quote_paths(&[path]))),
            Err(error) => {
                log::warn!("clipboard image not saved: {error}");
                None
            }
        },
        TerminalHost::Remote { label } => Some(PathAttach::Refuse {
            title: PASTE_REFUSED,
            body: remote_paste_body(label.as_deref()),
        }),
    }
}

/// Where pasted images are written: the per-user temporary directory, which
/// macOS clears on its own schedule.
pub fn dir() -> PathBuf {
    std::env::temp_dir().join("phux-attachments")
}

/// Write `image` under `dir` as `paste-<millis>.<ext>`. Formats agents do not
/// read (TIFF, BMP, ...) are converted to PNG with `sips` when it is present.
pub fn save_image(image: &Image, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("paste-{stamp}.{}", extension(image.format)));
    std::fs::write(&path, &image.bytes)?;
    if agent_readable(image.format) {
        return Ok(path);
    }
    let png = path.with_extension("png");
    let converted = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "png"])
        .arg(&path)
        .arg("--out")
        .arg(&png)
        .output()
        .is_ok_and(|output| output.status.success());
    if converted {
        let _ = std::fs::remove_file(&path);
        Ok(png)
    } else {
        Ok(path)
    }
}

fn agent_readable(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Webp
    )
}

fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Ico => "ico",
        ImageFormat::Pnm => "pnm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::ExternalPaths;

    #[test]
    fn paths_are_one_shell_word_each() {
        let quoted = quote_paths(&[
            PathBuf::from("/tmp/plain.txt"),
            PathBuf::from("/tmp/has space.txt"),
        ]);
        assert_eq!(quoted, "/tmp/plain.txt '/tmp/has space.txt'");
        assert!(!quoted.contains('\n'));
    }

    #[test]
    fn copied_files_paste_as_paths_not_their_names() {
        let item = ClipboardItem {
            entries: vec![
                ClipboardEntry::ExternalPaths(ExternalPaths(
                    vec![PathBuf::from("/tmp/shot one.png")].into(),
                )),
                ClipboardEntry::String(gpui::ClipboardString::new("shot one.png".into())),
            ],
        };
        assert_eq!(
            clipboard_paste(&item).as_deref(),
            Some("'/tmp/shot one.png'")
        );
    }

    #[test]
    fn text_wins_over_an_image() {
        let item = ClipboardItem {
            entries: vec![
                ClipboardEntry::String(gpui::ClipboardString::new("hello".into())),
                ClipboardEntry::Image(Image::from_bytes(ImageFormat::Png, vec![1, 2, 3])),
            ],
        };
        assert_eq!(clipboard_paste(&item).as_deref(), Some("hello"));
    }

    #[test]
    fn a_local_terminal_pastes_quoted_paths() {
        let host = terminal_host(None, &ResourceId::local(1));
        assert_eq!(host, TerminalHost::Local);
        assert_eq!(
            attach_paths(
                &host,
                &[
                    PathBuf::from("/tmp/plain.txt"),
                    PathBuf::from("/tmp/has space.txt"),
                ]
            ),
            Some(PathAttach::Paste(
                "/tmp/plain.txt '/tmp/has space.txt'".into()
            ))
        );
        assert_eq!(attach_paths(&host, &[]), None);
    }

    /// A remote server's own terminals use `ResourceId::Local`, and they can
    /// be input-ready. The id and readiness are not "this Mac".
    #[test]
    fn a_ready_remote_server_does_not_paste_local_paths() {
        let host = terminal_host(Some("mini"), &ResourceId::local(7));
        let decision = attach_paths(&host, &[PathBuf::from("/Users/me/Notes.txt")]);
        match decision {
            Some(PathAttach::Refuse { title, body }) => {
                assert_eq!(title, PASTE_REFUSED);
                assert!(body.contains("mini"), "{body}");
                assert!(!body.contains("/Users/me/Notes.txt"), "{body}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(matches!(
            terminal_host(Some("  "), &ResourceId::local(1)),
            TerminalHost::Local
        ));
    }

    #[test]
    fn a_satellite_terminal_on_the_local_server_is_remote() {
        let host = terminal_host(None, &ResourceId::satellite("box:2222", 3));
        let decision = attach_paths(&host, &[PathBuf::from("/tmp/a")]);
        match decision {
            Some(PathAttach::Refuse { body, .. }) => {
                assert!(body.contains("box:2222"), "{body}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn copied_files_on_a_remote_terminal_are_refused_and_text_still_pastes() {
        let files = ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
                vec![PathBuf::from("/tmp/shot one.png")].into(),
            ))],
        };
        let remote = terminal_host(Some("mini"), &ResourceId::local(1));
        assert!(matches!(
            clipboard_for_host(&files, &remote),
            Some(PathAttach::Refuse {
                title: PASTE_REFUSED,
                ..
            })
        ));
        assert_eq!(
            clipboard_for_host(&files, &TerminalHost::Local),
            Some(PathAttach::Paste("'/tmp/shot one.png'".into()))
        );

        let text = ClipboardItem {
            entries: vec![ClipboardEntry::String(gpui::ClipboardString::new(
                "hello".into(),
            ))],
        };
        assert_eq!(
            clipboard_for_host(&text, &remote),
            Some(PathAttach::Paste("hello".into()))
        );
    }

    #[test]
    fn a_copied_image_is_not_written_for_a_remote_terminal() {
        let item = ClipboardItem {
            entries: vec![ClipboardEntry::Image(Image::from_bytes(
                ImageFormat::Png,
                vec![1, 2, 3],
            ))],
        };
        let remote = terminal_host(Some("mini"), &ResourceId::local(1));
        assert!(matches!(
            clipboard_for_host(&item, &remote),
            Some(PathAttach::Refuse { .. })
        ));
    }

    #[test]
    fn an_image_is_saved_with_its_format() {
        let dir = std::env::temp_dir().join(format!("phux-attach-test-{}", std::process::id()));
        let image = Image::from_bytes(ImageFormat::Png, vec![137, 80, 78, 71]);
        let path = save_image(&image, &dir).expect("saved");
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("png"));
        assert_eq!(std::fs::read(&path).expect("read"), vec![137, 80, 78, 71]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
