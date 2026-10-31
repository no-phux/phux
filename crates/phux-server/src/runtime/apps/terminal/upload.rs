//! Sandboxed, acknowledged file uploads (`Command::PutFile`, ADR-0059).
//!
//! The client never chooses a path. A non-zero upload id maps to one partial
//! file under the server-owned upload directory; the requested extension is
//! restricted to a short ASCII token. Chunks are offset-checked and replay-safe,
//! and the final file becomes visible only after whole-file SHA-256 verification.
//!
//! An upload id is bound to the principal that created it (its credential,
//! else its local peer uid): another principal can neither append to it nor
//! `TRANSCRIBE` it. The directory as a whole is held to a byte and file quota
//! ([`UploadLimits`]), and partial uploads untouched for
//! [`PARTIAL_UPLOAD_TTL`] are swept at startup and on every upload. Completed
//! files are never deleted (ADR-0059): a full quota refuses new uploads with
//! the remedy instead.

use phux_terminal::actor::TerminalResource as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use phux_protocol::ids::{FileUploadId, ResourceId};
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode, MAX_FILE_UPLOAD_SIZE};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::state::{ClientId, ServerState, SharedState};

mod chunk;
mod quota;

use chunk::write_chunk;
use quota::read_metadata;
pub(in crate::runtime) use quota::sweep_stale_partials_at_startup;

/// The server is single-threaded, but per-client async tasks may interleave.
/// File operations contain no await, so one process-wide lock makes offset
/// checks + writes atomic without holding the registry lock across disk I/O.
static UPLOAD_LOCK: Mutex<()> = Mutex::new(());
const MAX_EXTENSION_LEN: usize = 16;
// Includes work waiting in spawn_blocking and work already running. Payload
// admission is independent of the protocol's per-frame maximum; a disconnected
// caller cannot release a running disk worker's reservation.
static UPLOAD_BUDGET: UploadBudget = UploadBudget::new(16, 32 * 1024 * 1024);

/// Default total bytes the upload directory may hold
/// (`PHUX_UPLOAD_MAX_BYTES`).
pub(in crate::runtime) const DEFAULT_UPLOAD_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Default number of uploads, finished or partial, the upload directory may
/// hold (`PHUX_UPLOAD_MAX_FILES`).
pub(in crate::runtime) const DEFAULT_UPLOAD_MAX_FILES: u64 = 10_000;
/// How long a partial upload may sit untouched before it is swept. A client
/// resumes within seconds of a reconnect; a day covers a phone left offline
/// overnight.
pub(in crate::runtime) const PARTIAL_UPLOAD_TTL: Duration = Duration::from_hours(24);

/// The upload directory's quota. `0` disables a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) struct UploadLimits {
    /// Total bytes of finished and partial uploads.
    pub max_bytes: u64,
    /// Finished plus partial uploads.
    pub max_files: u64,
    /// Age past which an untouched partial upload is swept.
    pub partial_ttl: Duration,
}

impl UploadLimits {
    /// The limits `env` configures, else the defaults.
    pub(in crate::runtime) fn from_env(env: &crate::runtime::ServerEnv) -> Self {
        Self {
            max_bytes: env.upload_max_bytes.unwrap_or(DEFAULT_UPLOAD_MAX_BYTES),
            max_files: env.upload_max_files.unwrap_or(DEFAULT_UPLOAD_MAX_FILES),
            partial_ttl: PARTIAL_UPLOAD_TTL,
        }
    }
}

/// The principal tag an upload id is bound to: a hash of the connection's
/// credential id, else of its kernel peer uid on the owner socket, else of
/// `anonymous` (an uncredentialed network listener). Hashed so the sidecar
/// names no credential. Every hub consumer reaches a satellite as the link's
/// one credential, so they share one principal there.
pub(in crate::runtime) fn upload_principal(server: &ServerState, client_id: ClientId) -> String {
    use phux_protocol::policy::TransportType;
    let socket_peer = || match server.peer_identity(client_id) {
        Some(peer) if peer.transport == TransportType::UnixSocket => format!("uid:{}", peer.uid),
        _ => "anonymous".to_owned(),
    };
    let principal = server
        .authenticated_credential(client_id)
        .map_or_else(socket_peer, |credential| {
            format!("credential:{}", credential.id)
        });
    hex::encode(&Sha256::digest(principal.as_bytes())[..16])
}

struct UploadBudget {
    jobs: Semaphore,
    payload: Semaphore,
}

struct UploadReservation {
    _job: SemaphorePermit<'static>,
    _payload: SemaphorePermit<'static>,
}

impl UploadBudget {
    const fn new(jobs: usize, payload_bytes: usize) -> Self {
        Self {
            jobs: Semaphore::const_new(jobs),
            payload: Semaphore::const_new(payload_bytes),
        }
    }

    fn reserve(&'static self, chunk: &PutFileChunk) -> Result<UploadReservation, CommandResult> {
        let retained = chunk
            .data
            .capacity()
            .saturating_add(chunk.extension.capacity());
        let bytes = u32::try_from(retained).map_err(|_| upload_busy())?;
        let job = self.jobs.try_acquire().map_err(|_| upload_busy())?;
        let payload = self
            .payload
            .try_acquire_many(bytes)
            .map_err(|_| upload_busy())?;
        Ok(UploadReservation {
            _job: job,
            _payload: payload,
        })
    }
}

fn upload_busy() -> CommandResult {
    error(
        ErrorCode::ResourceExhausted,
        "file upload worker budget exhausted; retry this chunk",
    )
}

pub(in crate::runtime) struct PutFileChunk {
    pub upload_id: FileUploadId,
    pub terminal_id: ResourceId,
    pub extension: String,
    pub offset: u64,
    pub data: Vec<u8>,
    pub final_chunk: bool,
    pub sha256: Option<[u8; 32]>,
    /// [`upload_principal`] of the sender.
    pub principal: String,
}

pub(in crate::runtime) async fn handle_put_file(
    state: &SharedState,
    chunk: PutFileChunk,
) -> CommandResult {
    // docs/protocol.md#commands-events-resources: PUT_FILE is a Terminal-facet command — the
    // upload lands beside a PTY's working directory, which an
    // `AgentSession` does not have.
    match state.with(|server| {
        server.terminal_from_wire(&chunk.terminal_id).map(|core| {
            server
                .resource_handle(core)
                .is_some_and(|h| h.terminal().is_ok())
        })
    }) {
        None => {
            return error(
                ErrorCode::TerminalNotFound,
                format!("no such terminal: {}", chunk.terminal_id),
            );
        }
        Some(false) => {
            return error(
                ErrorCode::WrongResourceKind,
                format!("not a Terminal: {}", chunk.terminal_id),
            );
        }
        Some(true) => {}
    }

    let env = state.with(ServerState::server_env);
    let root = match upload_dir(&env) {
        Ok(root) => root,
        Err(message) => return error(ErrorCode::InternalError, message),
    };
    run_upload(root, UploadLimits::from_env(&env), chunk).await
}

async fn run_upload(root: PathBuf, limits: UploadLimits, chunk: PutFileChunk) -> CommandResult {
    let reservation = match UPLOAD_BUDGET.reserve(&chunk) {
        Ok(reservation) => reservation,
        Err(result) => return result,
    };
    match tokio::task::spawn_blocking(move || {
        let _reservation = reservation;
        hold_admitted_upload_for_tests();
        let _guard = UPLOAD_LOCK.lock().map_err(|_| {
            (
                ErrorCode::InternalError,
                "file upload lock poisoned".to_owned(),
            )
        })?;
        let result = write_chunk(&root, limits, &chunk);
        drop(chunk);
        result
    })
    .await
    {
        Ok(Ok(ack)) => CommandResult::OkWith(CommandValue::FileUpload(ack)),
        Ok(Err((code, message))) => error(code, message),
        Err(join_error) => error(
            ErrorCode::InternalError,
            format!("file upload worker failed: {join_error}"),
        ),
    }
}

/// Testkit barrier (phux-sk2g): park an already-admitted disk worker until
/// `$PHUX_TEST_UPLOAD_HOLD/release` exists so a public-connection test can
/// prove accept/HELLO still run. Absent the env var, this is a no-op.
fn hold_admitted_upload_for_tests() {
    let Some(dir) = std::env::var_os("PHUX_TEST_UPLOAD_HOLD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join("held"), []);
    let release = dir.join("release");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !release.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.into(),
    }
}

/// The landed file of a finished upload, if `PUT_FILE` completed it.
///
/// Looked up by id rather than by a client-supplied path so a caller can
/// never name a file outside the sandbox: the id is hex-encoded server-side
/// and matched against `phux-upload-<hex>.<ext>` in the upload root. An
/// upload another principal created reads as absent, so its existence is not
/// disclosed.
pub(in crate::runtime) fn completed_upload_path(
    env: &crate::runtime::ServerEnv,
    upload_id: FileUploadId,
    principal: &str,
) -> Result<Option<PathBuf>, String> {
    let root = upload_dir(env)?;
    let id = hex::encode(upload_id.as_bytes());
    match read_metadata(&root.join(format!(".phux-upload-{id}.meta"))) {
        Ok(Some(meta)) if !meta.admits(principal) => return Ok(None),
        Ok(_) => {}
        Err((_, message)) => return Err(message),
    }
    let prefix = format!("phux-upload-{id}.");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read upload directory: {err}")),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&prefix) {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// The upload directory, refused to a development build when it is the
/// production one (it is not profile-scoped).
fn upload_dir(env: &crate::runtime::ServerEnv) -> Result<PathBuf, String> {
    let dir = unguarded_upload_dir(env)?;
    phux_config::production::refuse_dev_on_production_state(&dir)?;
    Ok(dir)
}

/// `PHUX_UPLOAD_DIR` ([`crate::runtime::ServerEnv::upload_dir`]), else the data
/// directory's `phux/uploads`.
fn unguarded_upload_dir(env: &crate::runtime::ServerEnv) -> Result<PathBuf, String> {
    if let Some(path) = &env.upload_dir {
        return Ok(path.clone());
    }
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path).join("phux").join("uploads"));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("phux")
                .join("uploads")
        })
        .ok_or_else(|| {
            "file upload directory unavailable: neither HOME nor XDG_DATA_HOME is set".to_owned()
        })
}

pub(in crate::runtime) fn digest_reader(
    reader: &mut impl Read,
) -> Result<sha2::digest::Output<Sha256>, (ErrorCode, String)> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize())
}

fn path_string(path: &Path) -> Result<String, (ErrorCode, String)> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        (
            ErrorCode::InternalError,
            "file upload path is not valid UTF-8".to_owned(),
        )
    })
}

fn upload_too_large() -> (ErrorCode, String) {
    (
        ErrorCode::ResourceExhausted,
        format!("file upload exceeds the {MAX_FILE_UPLOAD_SIZE} byte limit"),
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the owned signature is required for direct use with Result::map_err"
)]
fn io_error(err: std::io::Error) -> (ErrorCode, String) {
    (
        ErrorCode::InternalError,
        format!("file upload I/O failed: {err}"),
    )
}

#[cfg(test)]
mod tests;
