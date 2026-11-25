//! terminal-browser's newline-delimited terminal socket protocol, version 1.
//! This is an adapter over the public headless client, not another server API.

use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;

mod operations;
mod panes;

pub(crate) use phux_terminal_api::launch::TERMINAL_BROWSER_SOCKET_ENV as SOCKET_ENV;
const MAX_REQUEST: u64 = 256 * 1024;
const DEADLINE: Duration = Duration::from_secs(7);
const MAX_CONNECTIONS: usize = 32;
const METHODS: &[&str] = &[
    "getCurrentPane",
    "split",
    "listPanes",
    "neighbor",
    "sendText",
    "pasteKey",
];

#[derive(Deserialize)]
struct Request {
    id: String,
    method: String,
    #[serde(default)]
    params: Value,
    caller: Caller,
}

#[derive(Deserialize)]
struct Caller {
    pid: u32,
    cwd: String,
}

/// Deterministic across upgrades and distinct for every coordinator socket.
fn socket_path(server: &Path) -> PathBuf {
    let hash = Sha256::digest(server.as_os_str().as_encoded_bytes());
    let mut prefix = [0; 8];
    prefix.copy_from_slice(&hash[..8]);
    server.with_file_name(format!("tb-{:016x}.sock", u64::from_be_bytes(prefix)))
}

pub(crate) fn install(server: &Path) {
    match start(server) {
        Ok(path) => phux_terminal_api::launch::set_pane_env(vec![(SOCKET_ENV.into(), path.into())]),
        Err(error) => tracing::warn!("terminal-browser integration unavailable: {error}"),
    }
}

/// Called only after the coordinator has acquired its own listening socket.
/// A second, refused server must never unlink the running adapter.
pub(crate) fn start(server: &Path) -> io::Result<PathBuf> {
    let server = std::path::absolute(server)?;
    let path = socket_path(&server);
    remove_owned_socket(&path)?;
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    let guard = SocketGuard {
        identity: std::fs::symlink_metadata(&path)?,
        path: path.clone(),
    };
    tokio::spawn(serve(listener, server, guard));
    Ok(path)
}

fn remove_owned_socket(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "terminal-browser endpoint is not an owned socket",
        ));
    }
    std::fs::remove_file(path)
}

struct SocketGuard {
    path: PathBuf,
    identity: std::fs::Metadata,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.dev() == self.identity.dev() && metadata.ino() == self.identity.ino()
        }) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn serve(listener: UnixListener, server: PathBuf, _guard: SocketGuard) {
    let mut requests = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break };
                if requests.len() >= MAX_CONNECTIONS || !same_user(&stream) {
                    continue;
                }
                let server = server.clone();
                requests.spawn(async move {
                    let _ = tokio::time::timeout(DEADLINE, answer(stream, &server)).await;
                });
            }
            _ = requests.join_next(), if !requests.is_empty() => {}
        }
    }
}

fn same_user(stream: &UnixStream) -> bool {
    stream
        .peer_cred()
        .is_ok_and(|peer| peer.uid() == rustix::process::geteuid().as_raw())
}

async fn answer(mut stream: UnixStream, server: &Path) -> io::Result<()> {
    let mut line = Vec::new();
    BufReader::new((&mut stream).take(MAX_REQUEST + 1))
        .read_until(b'\n', &mut line)
        .await?;
    if line.len() as u64 > MAX_REQUEST || line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request line",
        ));
    }
    let parsed = serde_json::from_slice::<Value>(&line).unwrap_or(Value::Null);
    let reply = match serde_json::from_value::<Request>(parsed.clone()) {
        Ok(request) => {
            let result = operations::dispatch(server, &request).await;
            match result {
                Ok(result) => json!({ "id": request.id, "result": result }),
                Err(message) => json!({ "id": request.id, "error": { "message": message } }),
            }
        }
        Err(_) => json!({
            "id": parsed.get("id").and_then(Value::as_str).unwrap_or(""),
            "error": { "message": "invalid terminal-browser request" }
        }),
    };
    let mut bytes = serde_json::to_vec(&reply)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests;
