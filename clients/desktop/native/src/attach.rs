//! Attachments: dropped files and clipboard content become text a shell or a
//! coding agent reads. On this machine, files paste as shell-quoted paths and
//! a clipboard image (a screenshot, a copied picture) is written to a file
//! whose path is pasted, which agents such as Claude Code and Codex read as
//! an image attachment.
//!
//! A terminal on another host does not receive those Mac paths. The bytes go
//! through [`ControlPlane::put_file`](phux_client_runtime::control::ControlPlane::put_file),
//! and only the server path is pasted, after the receipt, when the destination
//! terminal and host are still the ones the upload started on. Readiness
//! (`Connection::ready`, `input_ready`) is not an input: a host that can take
//! keystrokes is not this machine.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat};
use phux_client_runtime::control::{FileUploadOutcome, FileUploadReceipt};
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::MAX_FILE_UPLOAD_SIZE;

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

/// Local files to upload. Their paths are never pasted into a remote terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadBatch {
    /// Paths on this machine. Each regular file is read, then uploaded.
    Paths(Vec<PathBuf>),
    /// Bytes already in memory, such as a clipboard image. No local path.
    Bytes {
        /// 1–16 ASCII letters or digits, or `bin`.
        extension: String,
        /// File contents. Not a path.
        bytes: Vec<u8>,
    },
}

/// A path paste the user can see, an upload, or a refusal to show instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathAttach {
    /// Shell-quoted text to paste. Local paths only.
    Paste(String),
    /// Upload, then paste the server path if the destination is unchanged.
    Upload(UploadBatch),
    /// Do not paste. Show this notice.
    Refuse {
        /// Notification title.
        title: &'static str,
        /// Notification body.
        body: String,
    },
}

/// The terminal and host an upload was addressed to.
///
/// `host` comes from [`terminal_host`]. It is not derived from input readiness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadDestination {
    /// Terminal the bytes were sent to.
    pub terminal: ResourceId,
    /// Filesystem host of that terminal when the upload started.
    pub host: TerminalHost,
}

/// What to do with one upload receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadFollowup {
    /// Paste this shell-quoted server path into `terminal`.
    Paste {
        /// The terminal the upload was addressed to.
        terminal: ResourceId,
        /// Quoted remote path. Not a path from this machine.
        text: String,
    },
    /// Do not paste. Show this notice.
    Notice {
        /// Notification title.
        title: &'static str,
        /// Notification body.
        body: String,
    },
}

/// One file ready for `put_file`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedUpload {
    /// Extension the runtime will accept.
    pub extension: String,
    /// File bytes.
    pub bytes: Vec<u8>,
    /// Local path these bytes were read from, when they came from a path.
    /// Compared against the receipt so that path is not pasted.
    pub local_source: Option<PathBuf>,
}

/// Why one file was not uploaded. Nothing was pasted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadPrepareError {
    /// Notification title.
    pub title: &'static str,
    /// Notification body.
    pub body: String,
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

/// Quoted paths for a local terminal. A remote terminal yields an upload and
/// no paste text. Empty `paths` is nothing to do.
#[must_use]
pub fn attach_paths(host: &TerminalHost, paths: &[PathBuf]) -> Option<PathAttach> {
    if paths.is_empty() {
        return None;
    }
    match host {
        TerminalHost::Local => Some(PathAttach::Paste(quote_paths(paths))),
        TerminalHost::Remote { .. } => Some(PathAttach::Upload(UploadBatch::Paths(paths.to_vec()))),
    }
}

/// Whether `current` is still the terminal and host `started` named.
///
/// A missing terminal, a different id, or a different host is a change.
/// Input readiness is not an argument and cannot make two hosts the same.
#[must_use]
pub fn same_upload_destination(
    started: &UploadDestination,
    current: Option<&UploadDestination>,
) -> bool {
    current
        .is_some_and(|current| current.terminal == started.terminal && current.host == started.host)
}

/// Decide the paste after one upload receipt.
///
/// The quoted text is the server path. `local_source`, when this upload was
/// read from a file on this machine, is refused if the receipt names it.
/// A changed destination, a refusal, an unknown outcome, or a missing path
/// pastes nothing.
#[must_use]
pub fn follow_upload(
    started: &UploadDestination,
    current: Option<&UploadDestination>,
    receipt: &FileUploadReceipt,
    local_source: Option<&Path>,
) -> UploadFollowup {
    if !same_upload_destination(started, current) {
        let body = match current {
            Some(current) if current.terminal == started.terminal => {
                "This terminal is no longer on the same machine. Nothing was pasted.".to_owned()
            }
            _ => "The terminal changed before the upload finished. Nothing was pasted.".to_owned(),
        };
        return UploadFollowup::Notice {
            title: PASTE_REFUSED,
            body,
        };
    }
    if receipt.outcome != FileUploadOutcome::Completed {
        return UploadFollowup::Notice {
            title: PASTE_REFUSED,
            body: unfinished(&receipt.message),
        };
    }
    let Some(path) = receipt.path.as_deref().filter(|path| !path.is_empty()) else {
        return UploadFollowup::Notice {
            title: PASTE_REFUSED,
            body: "The upload finished without a remote path. Nothing was pasted.".to_owned(),
        };
    };
    if local_source.is_some_and(|source| source.display().to_string() == path) {
        return UploadFollowup::Notice {
            title: PASTE_REFUSED,
            body: "The upload did not return a remote path. Nothing was pasted.".to_owned(),
        };
    }
    if path.chars().any(is_terminal_control) {
        return UploadFollowup::Notice {
            title: PASTE_REFUSED,
            body: "The remote path could not be pasted safely. Nothing was pasted.".to_owned(),
        };
    }
    UploadFollowup::Paste {
        terminal: started.terminal.clone(),
        text: quote_paths(&[PathBuf::from(path)]),
    }
}

fn unfinished(message: &str) -> String {
    let message = message.trim().trim_end_matches('.');
    if message.is_empty() {
        "The upload did not finish. Nothing was pasted.".to_owned()
    } else {
        format!("{message}. Nothing was pasted.")
    }
}

fn is_terminal_control(character: char) -> bool {
    let code = character as u32;
    code < 0x20 || (0x7f..=0x9f).contains(&code)
}

/// Read a drop or clipboard batch into `put_file` payloads.
///
/// One result per path, in order. In-memory bytes are one result. A directory,
/// a special file, or a file over 64 MiB is a notice and is not uploaded.
#[must_use]
pub fn prepare_upload(batch: UploadBatch) -> Vec<Result<PreparedUpload, UploadPrepareError>> {
    match batch {
        UploadBatch::Bytes { extension, bytes } => {
            vec![prepare_bytes(extension, bytes, None)]
        }
        UploadBatch::Paths(paths) => paths.iter().map(|path| prepare_path(path)).collect(),
    }
}

fn prepare_error(body: String) -> UploadPrepareError {
    UploadPrepareError {
        title: PASTE_REFUSED,
        body,
    }
}

fn prepare_path(path: &Path) -> Result<PreparedUpload, UploadPrepareError> {
    let name = file_name(path);
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Err(prepare_error(format!(
                "Could not read {name}. Nothing was pasted. ({error})"
            )));
        }
    };
    if !metadata.is_file() {
        return Err(prepare_error(format!(
            "{name} is not a file. Nothing was pasted."
        )));
    }
    if metadata.len() > MAX_FILE_UPLOAD_SIZE {
        return Err(prepare_error(format!(
            "{name} is over 64 MiB. Nothing was pasted."
        )));
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(prepare_error(format!(
                "Could not read {name}. Nothing was pasted. ({error})"
            )));
        }
    };
    prepare_bytes(infer_extension(path), bytes, Some(path.to_path_buf()))
}

fn prepare_bytes(
    extension: String,
    bytes: Vec<u8>,
    local_source: Option<PathBuf>,
) -> Result<PreparedUpload, UploadPrepareError> {
    let name = local_source
        .as_ref()
        .map(|path| file_name(path))
        .unwrap_or_else(|| "The image".to_owned());
    if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_FILE_UPLOAD_SIZE) {
        return Err(prepare_error(format!(
            "{name} is over 64 MiB. Nothing was pasted."
        )));
    }
    Ok(PreparedUpload {
        extension: canonical_extension(&extension),
        bytes,
        local_source,
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("That file")
        .to_owned()
}

/// A 1–16 character ASCII alphanumeric extension, or `bin`.
#[must_use]
pub fn infer_extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .map(canonical_extension)
        .unwrap_or_else(|| "bin".to_owned())
}

fn canonical_extension(value: &str) -> String {
    if !value.is_empty()
        && value.len() <= 16
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        value.to_owned()
    } else {
        "bin".to_owned()
    }
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
/// Remote terminals go through [`clipboard_for_host`], which uploads file and
/// image bytes instead of pasting paths from this machine.
pub fn clipboard_paste(item: &ClipboardItem) -> Option<String> {
    match clipboard_for_host(item, &TerminalHost::Local)? {
        PathAttach::Paste(text) => Some(text),
        PathAttach::Upload(_) | PathAttach::Refuse { .. } => None,
    }
}

/// Clipboard text is host-independent. Copied files and images on a remote
/// terminal are uploads: the Mac path is not pasted, and an image is not
/// written just to quote that path.
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
        TerminalHost::Remote { .. } => Some(remote_image(image)),
    }
}

/// Upload a clipboard image. Agent-readable bytes stay in memory. Other
/// formats are converted the same way a local paste converts them, and that
/// staging file is an upload source, not paste text.
fn remote_image(image: &Image) -> PathAttach {
    if agent_readable(image.format) {
        return PathAttach::Upload(UploadBatch::Bytes {
            extension: extension(image.format).to_owned(),
            bytes: image.bytes.clone(),
        });
    }
    match save_image(image, &dir()) {
        Ok(path) => PathAttach::Upload(UploadBatch::Paths(vec![path])),
        Err(error) => {
            log::warn!("clipboard image not saved: {error}");
            PathAttach::Refuse {
                title: PASTE_REFUSED,
                body: "The image could not be prepared. Nothing was pasted.".to_owned(),
            }
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

    /// A dropped file named like a command stays one quoted word: no newline
    /// outside quotes, no expansion, no command separator the shell can act on.
    #[test]
    fn a_hostile_dropped_name_is_pasted_quoted_and_never_executed() {
        let hostile = PathBuf::from("/tmp/x'; rm -rf ~; echo $(id) `id`\nreboot");
        let host = terminal_host(None, &ResourceId::local(1));
        let Some(PathAttach::Paste(text)) = attach_paths(&host, std::slice::from_ref(&hostile))
        else {
            panic!("a local drop pastes");
        };
        assert_eq!(
            text, "'/tmp/x'\"'\"'; rm -rf ~; echo $(id) `id`\nreboot'",
            "single-quoted with the embedded quote closed, escaped and reopened"
        );
        assert!(!text.ends_with('\n') && !text.ends_with('\r'), "no Enter");
        // Undo POSIX single-quote quoting: what the shell would read is the
        // file name itself, as one word.
        let unquoted = text
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap()
            .replace(r#"'"'"'"#, "'");
        assert_eq!(PathBuf::from(unquoted), hostile);
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
    /// be input-ready. The id and readiness are not "this Mac", so the drop
    /// is an upload of the local file rather than a paste of its path.
    #[test]
    fn a_ready_remote_server_uploads_instead_of_pasting_local_paths() {
        let host = terminal_host(Some("mini"), &ResourceId::local(7));
        assert!(matches!(host, TerminalHost::Remote { .. }));
        let source = PathBuf::from("/Users/me/Notes.txt");
        let decision = attach_paths(&host, std::slice::from_ref(&source));
        assert_eq!(
            decision,
            Some(PathAttach::Upload(UploadBatch::Paths(vec![source])))
        );
        assert!(matches!(
            terminal_host(Some("  "), &ResourceId::local(1)),
            TerminalHost::Local
        ));
    }

    #[test]
    fn a_satellite_terminal_on_the_local_server_is_an_upload() {
        let host = terminal_host(None, &ResourceId::satellite("box:2222", 3));
        let source = PathBuf::from("/tmp/a");
        let decision = attach_paths(&host, std::slice::from_ref(&source));
        assert_eq!(
            decision,
            Some(PathAttach::Upload(UploadBatch::Paths(vec![source])))
        );
        assert!(matches!(
            host,
            TerminalHost::Remote {
                label: Some(label)
            } if label == "box:2222"
        ));
    }

    #[test]
    fn copied_files_on_a_remote_terminal_upload_and_text_still_pastes() {
        let files = ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
                vec![PathBuf::from("/tmp/shot one.png")].into(),
            ))],
        };
        let remote = terminal_host(Some("mini"), &ResourceId::local(1));
        assert_eq!(
            clipboard_for_host(&files, &remote),
            Some(PathAttach::Upload(UploadBatch::Paths(vec![PathBuf::from(
                "/tmp/shot one.png"
            )])))
        );
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
    fn a_copied_image_on_a_remote_terminal_uploads_bytes_not_a_mac_path() {
        let item = ClipboardItem {
            entries: vec![ClipboardEntry::Image(Image::from_bytes(
                ImageFormat::Png,
                vec![1, 2, 3],
            ))],
        };
        let remote = terminal_host(Some("mini"), &ResourceId::local(1));
        assert_eq!(
            clipboard_for_host(&item, &remote),
            Some(PathAttach::Upload(UploadBatch::Bytes {
                extension: "png".into(),
                bytes: vec![1, 2, 3],
            }))
        );
    }

    fn remote_destination() -> UploadDestination {
        UploadDestination {
            terminal: ResourceId::local(7),
            host: TerminalHost::Remote {
                label: Some("mini".into()),
            },
        }
    }

    fn completed(path: &str) -> FileUploadReceipt {
        FileUploadReceipt {
            transfer_id: 4,
            outcome: FileUploadOutcome::Completed,
            path: Some(path.to_owned()),
            code: None,
            message: String::new(),
        }
    }

    #[test]
    fn the_same_terminal_and_host_paste_the_remote_path_not_the_local_file() {
        let started = remote_destination();
        let local = Path::new("/Users/me/Notes.txt");
        let follow = follow_upload(
            &started,
            Some(&started),
            &completed("/srv/phux/uploads/phux-upload-abc.png"),
            Some(local),
        );
        assert_eq!(
            follow,
            UploadFollowup::Paste {
                terminal: ResourceId::local(7),
                text: "/srv/phux/uploads/phux-upload-abc.png".into(),
            }
        );
        let spaced = follow_upload(
            &started,
            Some(&started),
            &completed("/srv/phux/uploads/my file.png"),
            Some(local),
        );
        assert_eq!(
            spaced,
            UploadFollowup::Paste {
                terminal: ResourceId::local(7),
                text: "'/srv/phux/uploads/my file.png'".into(),
            }
        );
    }

    /// `ResourceId::Local` on a remote server is still that server. Moving the
    /// window onto this machine, or onto another host, is a different filesystem
    /// even when the id and a hypothetical input-ready bit would match.
    #[test]
    fn a_changed_host_or_terminal_does_not_paste() {
        let started = remote_destination();
        let receipt = completed("/srv/phux/uploads/phux-upload-abc.png");
        let local_id_on_this_machine = UploadDestination {
            terminal: ResourceId::local(7),
            host: TerminalHost::Local,
        };
        assert!(!same_upload_destination(
            &started,
            Some(&local_id_on_this_machine)
        ));
        assert!(matches!(
            follow_upload(&started, Some(&local_id_on_this_machine), &receipt, None),
            UploadFollowup::Notice {
                title: PASTE_REFUSED,
                ..
            }
        ));

        let other_host = UploadDestination {
            terminal: ResourceId::local(7),
            host: TerminalHost::Remote {
                label: Some("other".into()),
            },
        };
        assert!(matches!(
            follow_upload(&started, Some(&other_host), &receipt, None),
            UploadFollowup::Notice { .. }
        ));

        let other_terminal = UploadDestination {
            terminal: ResourceId::local(8),
            host: started.host.clone(),
        };
        assert!(matches!(
            follow_upload(&started, Some(&other_terminal), &receipt, None),
            UploadFollowup::Notice { .. }
        ));
        assert!(matches!(
            follow_upload(&started, None, &receipt, None),
            UploadFollowup::Notice { .. }
        ));
    }

    #[test]
    fn a_failed_or_local_receipt_does_not_paste() {
        let started = remote_destination();
        let refused = FileUploadReceipt {
            transfer_id: 4,
            outcome: FileUploadOutcome::Refused,
            path: Some("/srv/phux/uploads/phux-upload-abc.png".into()),
            code: Some(1),
            message: "the server does not support file upload".into(),
        };
        match follow_upload(&started, Some(&started), &refused, None) {
            UploadFollowup::Notice { body, .. } => {
                assert!(body.contains("does not support"), "{body}");
                assert!(body.contains("Nothing was pasted"), "{body}");
                assert!(!body.contains("/Users"), "{body}");
            }
            UploadFollowup::Paste { text, .. } => panic!("pasted {text}"),
        }
        let unknown = FileUploadReceipt {
            outcome: FileUploadOutcome::Unknown,
            path: Some("/srv/phux/uploads/phux-upload-abc.png".into()),
            message: String::new(),
            ..refused
        };
        assert!(matches!(
            follow_upload(&started, Some(&started), &unknown, None),
            UploadFollowup::Notice { .. }
        ));
        let local_path = completed("/Users/me/Notes.txt");
        assert!(matches!(
            follow_upload(
                &started,
                Some(&started),
                &local_path,
                Some(Path::new("/Users/me/Notes.txt")),
            ),
            UploadFollowup::Notice { .. }
        ));
        let controls = completed("/srv/evil\npath");
        assert!(matches!(
            follow_upload(&started, Some(&started), &controls, None),
            UploadFollowup::Notice { .. }
        ));
    }

    #[test]
    fn prepare_upload_reads_a_file_and_refuses_a_directory_or_a_huge_file() {
        let dir = std::env::temp_dir().join(format!(
            "phux-upload-prep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"hello").expect("write");
        let folder = dir.join("subdir");
        std::fs::create_dir(&folder).expect("subdir");
        let huge = dir.join("huge.bin");
        let created = std::fs::File::create(&huge).expect("huge");
        created
            .set_len(MAX_FILE_UPLOAD_SIZE + 1)
            .expect("sparse length");

        let prepared = prepare_upload(UploadBatch::Paths(vec![file.clone(), folder.clone(), huge]));
        assert_eq!(prepared.len(), 3);
        assert_eq!(
            prepared[0].as_ref().expect("file"),
            &PreparedUpload {
                extension: "txt".into(),
                bytes: b"hello".to_vec(),
                local_source: Some(file),
            }
        );
        assert!(prepared[1].as_ref().is_err_and(|error| {
            error.body.contains("not a file") && !error.body.contains("pasted /")
        }));
        assert!(
            prepared[2]
                .as_ref()
                .is_err_and(|error| error.body.contains("64 MiB"))
        );
        assert_eq!(infer_extension(Path::new("/tmp/noext")), "bin");
        assert_eq!(infer_extension(Path::new("/tmp/a.tar.gz")), "gz");
        let _ = std::fs::remove_dir_all(&dir);
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
