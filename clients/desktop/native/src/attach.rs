//! Attachments: dropped files and clipboard content become text a shell or a
//! coding agent reads. Files paste as shell-quoted paths; a clipboard image
//! (a screenshot, a copied picture) is written to a file whose path is pasted,
//! which agents such as Claude Code and Codex read as an image attachment.
//!
//! Paths are local to this machine. A terminal on a remote host receives the
//! same local path text.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat};

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

/// What Paste sends for this clipboard item: copied files as quoted paths,
/// else the text, else an image saved under [`dir`] as its quoted path.
/// `None` when there is nothing to paste or the image could not be written.
pub fn clipboard_paste(item: &ClipboardItem) -> Option<String> {
    let entries = item.entries();
    if let Some(paths) = entries.iter().find_map(|entry| match entry {
        ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => Some(paths),
        _ => None,
    }) {
        return Some(quote_paths(paths.paths()));
    }
    if let Some(text) = item.text().filter(|text| !text.is_empty()) {
        return Some(text);
    }
    let image = entries.iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(image),
        _ => None,
    })?;
    match save_image(image, &dir()) {
        Ok(path) => Some(quote_paths(&[path])),
        Err(error) => {
            log::warn!("clipboard image not saved: {error}");
            None
        }
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
    fn an_image_is_saved_with_its_format() {
        let dir = std::env::temp_dir().join(format!("phux-attach-test-{}", std::process::id()));
        let image = Image::from_bytes(ImageFormat::Png, vec![137, 80, 78, 71]);
        let path = save_image(&image, &dir).expect("saved");
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("png"));
        assert_eq!(std::fs::read(&path).expect("read"), vec![137, 80, 78, 71]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
