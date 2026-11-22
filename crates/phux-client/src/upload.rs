//! Headless file uploads through the managed runtime.
//!
//! Uploading never attaches, resizes, pastes, or submits input. The returned
//! path belongs to the terminal-owning host, not necessarily this machine.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use phux_client_runtime::control::{ControlOptions, FileUploadOutcome, FileUploadReceipt};
use phux_client_runtime::runtime::{
    Client, ClientOptions, Listener, Runtime, RuntimeError, Target,
};
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::MAX_FILE_UPLOAD_SIZE;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio::time::{Instant, timeout_at};

/// Terminal upload outcome. Only `Completed` proves a retained file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadOutcome {
    /// The server acknowledged the complete file and supplied its path.
    Completed,
    /// The runtime or server refused the operation. This is not a guarantee
    /// that earlier chunks left no temporary artifacts.
    Refused,
    /// Some, all, or none of the bytes may have landed; do not blindly retry.
    Unknown,
}

/// Serializable result, retaining the exact owning terminal and wire detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadReceipt {
    /// Owning terminal, including its satellite namespace if present.
    pub terminal_id: ResourceId,
    /// Runtime-local correlation, not a portable server upload identifier.
    pub transfer_id: u64,
    /// Terminal disposition, including uncertain completion.
    pub outcome: UploadOutcome,
    /// Authoritative path on the owning host, or null without completion.
    pub path: Option<String>,
    /// Wire refusal code, if supplied.
    pub code: Option<u16>,
    /// Display-only diagnostic detail.
    pub message: String,
}

impl UploadReceipt {
    fn from_runtime(terminal_id: ResourceId, receipt: FileUploadReceipt) -> Self {
        Self {
            terminal_id,
            transfer_id: receipt.transfer_id,
            outcome: match receipt.outcome {
                FileUploadOutcome::Completed => UploadOutcome::Completed,
                FileUploadOutcome::Refused => UploadOutcome::Refused,
                FileUploadOutcome::Unknown => UploadOutcome::Unknown,
            },
            path: receipt.path,
            code: receipt.code,
            message: receipt.message,
        }
    }
}

/// Failure before a runtime-managed transfer was started.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    /// Local file I/O failed.
    #[error("could not read upload file: {0}")]
    Io(#[from] std::io::Error),
    /// The path does not resolve to a regular file.
    #[error("upload source must be a regular file")]
    NotRegularFile,
    /// The source exceeds the protocol's byte limit.
    #[error("file exceeds the 64 MiB upload limit")]
    TooLarge,
    /// The blocking file reader failed to run.
    #[error("upload file reader failed: {0}")]
    Reader(#[from] tokio::task::JoinError),
    /// The overall deadline elapsed before a transfer could start.
    #[error("upload deadline elapsed before the transfer started")]
    Deadline,
    /// The runtime's thread could not start.
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

/// Infer a 1–16 character ASCII alphanumeric extension, falling back to `bin`.
/// Explicit extensions passed to upload functions are validated by the runtime.
#[must_use]
pub fn infer_extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 16
                && value.as_bytes().iter().all(u8::is_ascii_alphanumeric)
        })
        .unwrap_or("bin")
        .to_owned()
}

/// Read a regular local file with a metadata check and a limit+1 bounded read.
///
/// The blocking reader may finish after this future is cancelled; it never
/// connects or uploads. Symlinks resolving to regular files are accepted.
pub async fn load_file(path: PathBuf) -> Result<Vec<u8>, UploadError> {
    tokio::task::spawn_blocking(move || {
        check_file(&std::fs::metadata(&path)?)?;
        let file = std::fs::File::open(path)?;
        check_file(&file.metadata()?)?;
        let mut data = Vec::new();
        file.take(MAX_FILE_UPLOAD_SIZE + 1).read_to_end(&mut data)?;
        if u64::try_from(data.len()).map_or(true, |len| len > MAX_FILE_UPLOAD_SIZE) {
            return Err(UploadError::TooLarge);
        }
        Ok(data)
    })
    .await?
}

fn check_file(metadata: &std::fs::Metadata) -> Result<(), UploadError> {
    if !metadata.is_file() {
        return Err(UploadError::NotRegularFile);
    }
    if metadata.len() > MAX_FILE_UPLOAD_SIZE {
        return Err(UploadError::TooLarge);
    }
    Ok(())
}

/// Upload a local file, including loading in the overall time budget.
///
/// `extension` overrides inference; invalid explicit values produce a refusal
/// receipt through runtime validation. No local filename is sent to the server.
pub async fn upload_file(
    target: Target,
    terminal_id: ResourceId,
    path: PathBuf,
    extension: Option<String>,
    budget: Duration,
) -> Result<UploadReceipt, UploadError> {
    let deadline = deadline(budget)?;
    if Instant::now() >= deadline {
        return Err(UploadError::Deadline);
    }
    let extension = extension.unwrap_or_else(|| infer_extension(&path));
    let data = timeout_at(deadline, load_file(path))
        .await
        .map_err(|_| UploadError::Deadline)??;
    upload_until(target, terminal_id, extension, data, deadline).await
}

/// Upload bytes with a bounded overall wait, preserving runtime chunking,
/// capability checks and reconnect replay.
///
/// The exact `terminal_id` is forwarded. On timeout the runtime closes and
/// reports `Refused` if never attempted, or `Unknown` otherwise.
/// Dropping/cancelling the future always closes its client.
pub async fn upload_bytes(
    target: Target,
    terminal_id: ResourceId,
    extension: String,
    data: Vec<u8>,
    budget: Duration,
) -> Result<UploadReceipt, UploadError> {
    upload_until(target, terminal_id, extension, data, deadline(budget)?).await
}

fn deadline(budget: Duration) -> Result<Instant, UploadError> {
    Instant::now()
        .checked_add(budget)
        .ok_or(UploadError::Deadline)
}

struct Activity(Notify);

impl Listener for Activity {
    fn on_activity(&self) {
        self.0.notify_one();
    }
}

struct UploadClient(Client);

impl Drop for UploadClient {
    fn drop(&mut self) {
        self.0.close();
    }
}

async fn upload_until(
    target: Target,
    terminal_id: ResourceId,
    extension: String,
    data: Vec<u8>,
    deadline: Instant,
) -> Result<UploadReceipt, UploadError> {
    if Instant::now() >= deadline {
        return Err(UploadError::Deadline);
    }
    let client = UploadClient(Runtime::connect(
        target,
        ClientOptions {
            control: ControlOptions {
                client_name: "phux-client-upload".to_owned(),
                attach: None,
                viewport: (0, 0),
                auto_attach_foreign_spawns: false,
                subscribe_agent_sessions: false,
                ..ControlOptions::default()
            },
            ..ClientOptions::default()
        },
    )?);
    let activity = Arc::new(Activity(Notify::new()));
    client.0.set_listener(activity.clone());
    let transfer_id = client.0.put_file(terminal_id.clone(), extension, data);
    let mut timed_out = false;
    loop {
        // Re-arm BEFORE reading receipts. notify_one retains a permit if an
        // edge arrives between these drains and registering the next waiter.
        let _ = client.0.take_events();
        if let Some(mut receipt) = client
            .0
            .take_file_upload_receipts()
            .into_iter()
            .find(|receipt| receipt.transfer_id == transfer_id)
        {
            if timed_out && receipt.outcome != FileUploadOutcome::Completed {
                receipt.message = format!("upload deadline elapsed; {}", receipt.message);
            }
            return Ok(UploadReceipt::from_runtime(terminal_id, receipt));
        }
        if Instant::now() >= deadline || timeout_at(deadline, activity.0.notified()).await.is_err()
        {
            // close synchronously strands pending uploads with the runtime's
            // attempted/unattempted distinction; drain that receipt next turn.
            client.0.close();
            timed_out = true;
        }
    }
}
