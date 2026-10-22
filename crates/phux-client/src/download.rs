//! Headless file downloads: `GET_FILE` chunks written to a local path and
//! verified against the server's whole-file digest.
//!
//! Downloading never attaches, resizes, pastes, or submits input. The remote
//! path belongs to the server you connect to, or to the satellite `host`
//! names; the local path is this machine.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FileChunk, MAX_FILE_UPLOAD_CHUNK,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, timeout_at};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Terminal download outcome. Only `Completed` proves a verified local file;
/// `Refused` leaves nothing at the local path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadOutcome {
    /// Every byte landed and the digest matched the server's.
    Completed,
    /// The server or the local disk refused; the partial file was removed.
    Refused,
}

/// Serializable result of one download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadReceipt {
    /// The satellite that was read, or null for the server itself.
    pub host: Option<String>,
    /// The path read on that host.
    pub remote_path: String,
    /// Where the file landed on this machine.
    pub local_path: PathBuf,
    /// Bytes written before the outcome.
    pub bytes: u64,
    /// Terminal disposition.
    pub outcome: DownloadOutcome,
    /// Wire refusal code, if the server supplied one.
    pub code: Option<u16>,
    /// Display-only diagnostic detail.
    pub message: String,
}

/// What the caller asks for.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// A satellite of the connected server, or `None` for the server itself.
    pub host: Option<String>,
    /// Absolute path on that host.
    pub remote_path: String,
    /// Destination file on this machine.
    pub local_path: PathBuf,
    /// Replace an existing `local_path`. Without it, an existing file is
    /// refused atomically at publication time, not by an earlier check.
    pub force: bool,
}

/// Download `request.remote_path` into `request.local_path`, publishing it
/// only once the digest verifies.
///
/// Bytes accumulate in a fresh, randomly named sibling that is created
/// exclusively (never through a pre-placed path or symlink) and removed on
/// every failure, including a transport error and the `deadline` elapsing.
///
/// # Errors
///
/// Transport and decode failures on `conn`, after the partial file is
/// removed; a server refusal is a `Refused` receipt, not an error.
pub async fn download_on(
    conn: &mut Connection,
    request: DownloadRequest,
    deadline: Instant,
) -> Result<DownloadReceipt, AttachError> {
    let mut transfer = Transfer {
        host: request.host,
        remote_path: request.remote_path,
        local_path: request.local_path,
        bytes: 0,
    };
    if !conn
        .server_features_ext()
        .contains(ServerFeatureExt::FileDownload)
    {
        return Ok(transfer.refused(
            None,
            "the server does not advertise FILE_DOWNLOAD; upgrade phux on that host",
        ));
    }
    let part = part_path(&transfer.local_path);
    let file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&part)
    {
        Ok(file) => file,
        Err(err) => {
            return Ok(transfer.refused(None, format!("cannot create {}: {err}", part.display())));
        }
    };
    let outcome = fetch(conn, &mut transfer, file, &part, request.force, deadline).await;
    match outcome {
        Ok(Ok(())) => Ok(transfer.completed()),
        Ok(Err(Refusal { code, message })) => {
            let _ = fs::remove_file(&part);
            Ok(transfer.refused(code, message))
        }
        Err(err) => {
            let _ = fs::remove_file(&part);
            Err(err)
        }
    }
}

#[derive(Debug)]
struct Refusal {
    code: Option<u16>,
    message: String,
}

fn refusal(message: impl Into<String>) -> Refusal {
    Refusal {
        code: None,
        message: message.into(),
    }
}

/// Pull every chunk into `file`, then publish it. `Err(AttachError)` is a
/// transport failure; `Ok(Err(_))` a refusal. Either way the caller removes
/// the part file.
async fn fetch(
    conn: &mut Connection,
    transfer: &mut Transfer,
    mut file: File,
    part: &Path,
    force: bool,
    deadline: Instant,
) -> Result<Result<(), Refusal>, AttachError> {
    let mut hasher = Sha256::new();
    let mut request_id = 1;
    loop {
        let command = Command::GetFile {
            host: transfer.host.clone(),
            path: transfer.remote_path.clone(),
            offset: transfer.bytes,
            max_bytes: CHUNK,
        };
        let Ok(reply) = timeout_at(deadline, conn.request(request_id, command)).await else {
            return Ok(Err(refusal("download deadline elapsed")));
        };
        request_id = request_id.wrapping_add(1);
        match reply?.into_result_ignoring_interleaved() {
            CommandResult::OkWith(CommandValue::FileChunk(chunk)) => {
                if let Err(refused) = write_chunk(&mut file, &mut hasher, transfer, &chunk) {
                    return Ok(Err(refused));
                }
                if let Some(expected) = chunk.sha256 {
                    if <[u8; 32]>::from(hasher.finalize()) != expected {
                        return Ok(Err(refusal(
                            "SHA-256 mismatch: the file changed during the download; retry",
                        )));
                    }
                    return Ok(publish(file, part, &transfer.local_path, force));
                }
                if chunk.data.is_empty() {
                    return Ok(Err(refusal(
                        "the server returned an empty chunk before the end of the file",
                    )));
                }
            }
            CommandResult::Error { code, message } => {
                return Ok(Err(Refusal {
                    code: Some(code.as_wire()),
                    message,
                }));
            }
            other => return Ok(Err(refusal(format!("unexpected reply: {other:?}")))),
        }
    }
}

/// Bytes asked for per round trip: the wire's chunk ceiling.
#[allow(
    clippy::cast_possible_truncation,
    reason = "MAX_FILE_UPLOAD_CHUNK is 8 MiB, far below u32::MAX"
)]
const CHUNK: u32 = MAX_FILE_UPLOAD_CHUNK as u32;

/// Append one chunk, refusing before any write when the running total would
/// exceed the server's reported size or, on the final chunk, not equal it.
fn write_chunk(
    file: &mut impl Write,
    hasher: &mut Sha256,
    transfer: &mut Transfer,
    chunk: &FileChunk,
) -> Result<(), Refusal> {
    let bytes = transfer
        .bytes
        .saturating_add(u64::try_from(chunk.data.len()).unwrap_or(u64::MAX));
    if bytes > chunk.size || (chunk.sha256.is_some() && bytes != chunk.size) {
        return Err(refusal(format!(
            "the server returned {bytes} bytes for a {}-byte file; retry",
            chunk.size
        )));
    }
    file.write_all(&chunk.data)
        .map_err(|err| refusal(format!("local write failed: {err}")))?;
    hasher.update(&chunk.data);
    transfer.bytes = bytes;
    Ok(())
}

/// A fresh sibling of `local` for the bytes in flight. Random so a
/// pre-placed file or symlink at a guessable name cannot be clobbered or
/// followed: the open is exclusive and fails if anything is there.
fn part_path(local: &Path) -> PathBuf {
    let mut name = local
        .file_name()
        .map_or_else(Default::default, ToOwned::to_owned);
    name.push(format!(".{}.phux-part", uuid::Uuid::new_v4().simple()));
    local.with_file_name(name)
}

/// Move the verified bytes into place. Without `force`, a hard link claims
/// the destination atomically and fails if anything already exists there, so
/// no check-then-rename window can replace a file that appeared in between.
fn publish(file: File, part: &Path, local: &Path, force: bool) -> Result<(), Refusal> {
    if let Err(err) = file.sync_all() {
        return Err(refusal(format!("cannot sync the downloaded bytes: {err}")));
    }
    drop(file);
    if force {
        return fs::rename(part, local)
            .map_err(|err| refusal(format!("cannot place {}: {err}", local.display())));
    }
    match fs::hard_link(part, local) {
        Ok(()) => {
            let _ = fs::remove_file(part);
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err(refusal(format!(
            "{} exists; pass --force to replace it",
            local.display()
        ))),
        Err(err) => Err(refusal(format!("cannot place {}: {err}", local.display()))),
    }
}

struct Transfer {
    host: Option<String>,
    remote_path: String,
    local_path: PathBuf,
    bytes: u64,
}

impl Transfer {
    fn completed(self) -> DownloadReceipt {
        self.receipt(DownloadOutcome::Completed, None, String::new())
    }

    fn refused(self, code: Option<u16>, message: impl Into<String>) -> DownloadReceipt {
        self.receipt(DownloadOutcome::Refused, code, message.into())
    }

    fn receipt(
        self,
        outcome: DownloadOutcome,
        code: Option<u16>,
        message: String,
    ) -> DownloadReceipt {
        DownloadReceipt {
            host: self.host,
            remote_path: self.remote_path,
            local_path: self.local_path,
            bytes: self.bytes,
            outcome,
            code,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_part_file_is_a_fresh_sibling_of_the_target() {
        let part = part_path(Path::new("/tmp/out/deck.pptx"));
        assert_eq!(part.parent(), Some(Path::new("/tmp/out")));
        let name = part.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("deck.pptx.") && name.ends_with(".phux-part"));
        assert_ne!(part, part_path(Path::new("/tmp/out/deck.pptx")));
    }

    #[test]
    fn publication_without_force_never_replaces_and_with_force_does() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("deck.pptx");
        std::fs::write(&local, b"old").unwrap();
        let part = part_path(&local);
        let file = File::create(&part).unwrap();
        std::fs::write(&part, b"new").unwrap();
        let refused = publish(file, &part, &local, false).unwrap_err();
        assert!(refused.message.contains("--force"), "{}", refused.message);
        assert_eq!(std::fs::read(&local).unwrap(), b"old");
        assert!(part.exists(), "the caller removes the part on refusal");
        let file = File::open(&part).unwrap();
        publish(file, &part, &local, true).unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"new");
        assert!(!part.exists());
    }

    #[test]
    fn chunks_inconsistent_with_the_reported_size_are_refused_before_writing() {
        let mut transfer = Transfer {
            host: None,
            remote_path: "/remote/file".into(),
            local_path: PathBuf::from("/tmp/out"),
            bytes: 0,
        };
        let overrun = FileChunk {
            size: 1,
            data: vec![1, 2],
            sha256: None,
        };
        let short_final = FileChunk {
            size: 2,
            data: vec![1],
            sha256: Some(Sha256::digest([1]).into()),
        };
        for chunk in [overrun, short_final] {
            let mut written = Vec::new();
            let refused =
                write_chunk(&mut written, &mut Sha256::new(), &mut transfer, &chunk).unwrap_err();
            assert!(
                refused.message.contains("-byte file"),
                "{}",
                refused.message
            );
            assert!(written.is_empty());
            assert_eq!(transfer.bytes, 0);
        }
        let exact = FileChunk {
            size: 1,
            data: vec![7],
            sha256: Some(Sha256::digest([7]).into()),
        };
        let mut written = Vec::new();
        write_chunk(&mut written, &mut Sha256::new(), &mut transfer, &exact).unwrap();
        assert_eq!((written, transfer.bytes), (vec![7], 1));
    }
}
