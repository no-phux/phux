//! `phux idea` — capture one idea and post it to the ideas endpoint.
//!
//! The idea is written to the profile state spool before any network call,
//! under a `UUIDv7` idempotency key that stays with those bytes until a 2xx.
//! A later `phux idea` flushes the spool first. The signature is
//! HMAC-SHA256 over the 32 bytes decoded from `PHUX_IDEAS_KEY`, not over
//! the hex text:
//! `ts + "\n" + idempotency key + "\n" + lowercase sha256 hex of the body`.
//! The same key with a different body is HTTP 409 and is not sent again.
//! Bodies and the secret are never logged or printed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// Inclusive body limit. One byte past this is refused locally and by the server.
const MAX_BODY_BYTES: usize = 32 * 1024;

/// Spool files larger than this are left in place rather than read whole.
const MAX_SPOOL_FILE: u64 = 256 * 1024;

/// Response bodies are small JSON. Anything past this is not a capture id.
const MAX_RESPONSE_BYTES: usize = 8 * 1024;

const CAPTURE_URL: &str = "https://ingest.phux.sh/ideas/capture";
const SOURCE_CLI: &str = "cli";
const CONFLICT_STATUS: u16 = 409;
const CONFLICT_REASON: &str = "idempotency conflict";

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// `$XDG_CONFIG_HOME/phux/ops.env`, else `~/.config/phux/ops.env`.
///
/// The same directory as [`phux_config::loader::config_path`].
#[must_use]
pub(crate) fn ops_env_path() -> PathBuf {
    let mut path = phux_config::loader::config_path();
    path.set_file_name("ops.env");
    path
}

/// Unsent captures: `$XDG_STATE_HOME/<profile-dir>/ideas/spool`.
#[must_use]
pub(crate) fn ideas_spool_dir() -> PathBuf {
    phux_config::instance::state_dir()
        .join("ideas")
        .join("spool")
}

/// `phux idea [TEXT] [--private] [--flush]`.
pub(crate) fn run(args: crate::commands::IdeaArgs) -> ExitCode {
    let crate::commands::IdeaArgs {
        private,
        flush,
        text,
    } = args;
    let text = text.as_deref();
    if private && text.is_none() {
        error!("--private needs the idea text");
        hint!("example: phux idea --private \"keep this off the triage model\"");
        return ExitCode::from(EXIT_USAGE);
    }
    if text.is_none() && !flush {
        error!("pass the idea text, or --flush to send ideas already queued");
        hint!("example: phux idea \"ship the spool before the post\"");
        return ExitCode::from(EXIT_USAGE);
    }
    let body = match text.map(|text| prepare_body(text, private)).transpose() {
        Ok(body) => body,
        Err(message) => {
            error!("{message}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let path = ops_env_path();
    let creds = match load_creds(&path) {
        Ok(creds) => creds,
        Err(err) => {
            error!("{err}");
            hint!(
                "PHUX_IDEAS_KEY_ID is the key id, for example cli-mbp. \
                 PHUX_IDEAS_KEY is 64 hex characters, the 32-byte HMAC secret. \
                 The secret is never printed."
            );
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match deliver(
        &ideas_spool_dir(),
        &creds,
        CAPTURE_URL,
        body.as_deref(),
        unix_now,
    ) {
        Ok(report) => report.emit(),
        Err(message) => {
            error!("{message}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The bytes that will be posted. `--private` ensures `#private` is present.
fn prepare_body(text: &str, private: bool) -> Result<String, String> {
    if text.is_empty() && !private {
        return Err("the idea text is empty".to_owned());
    }
    let body = if private && !text.contains("#private") {
        if text.is_empty() {
            "#private".to_owned()
        } else {
            format!("#private\n{text}")
        }
    } else {
        text.to_owned()
    };
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "the idea is {} bytes; ideas capture accepts at most {MAX_BODY_BYTES}",
            body.len()
        ));
    }
    Ok(body)
}

/// Key id plus the decoded 32-byte HMAC secret. `Debug` redacts the secret.
struct Creds {
    key_id: String,
    secret: [u8; 32],
}

impl std::fmt::Debug for Creds {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Creds")
            .field("key_id", &self.key_id)
            .field("secret", &"[redacted]")
            .finish()
    }
}

#[derive(Debug)]
enum CredsError {
    Io(String),
    Missing { path: PathBuf, id: bool, key: bool },
    BadKeyId { path: PathBuf },
    BadSecret { path: PathBuf },
}

impl std::fmt::Display for CredsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(message) => formatter.write_str(message),
            Self::Missing { path, id, key } => {
                let which = match (*id, *key) {
                    (false, true) => "PHUX_IDEAS_KEY_ID",
                    (true, false) => "PHUX_IDEAS_KEY",
                    (false, false) | (true, true) => "PHUX_IDEAS_KEY_ID and PHUX_IDEAS_KEY",
                };
                write!(
                    formatter,
                    "ideas capture needs {which} in {}",
                    path.display()
                )
            }
            Self::BadKeyId { path } => write!(
                formatter,
                "PHUX_IDEAS_KEY_ID in {} is not a valid key id. The value was not printed.",
                path.display()
            ),
            Self::BadSecret { path } => write!(
                formatter,
                "PHUX_IDEAS_KEY in {} must be 64 hex characters (32 bytes). The value was not printed.",
                path.display()
            ),
        }
    }
}

fn load_creds(path: &Path) -> Result<Creds, CredsError> {
    let text = read_env_file(path)?;
    let mut key_id = None;
    let mut secret = None;
    for line in text.lines() {
        let Some((key, value)) = assignment(line) else {
            continue;
        };
        if key == "PHUX_IDEAS_KEY_ID" {
            key_id = Some(value.to_owned());
        } else if key == "PHUX_IDEAS_KEY" {
            secret = Some(value.to_owned());
        }
    }
    let id_present = key_id.is_some();
    let key_present = secret.is_some();
    let (Some(key_id), Some(secret_hex)) = (key_id, secret) else {
        return Err(CredsError::Missing {
            path: path.to_owned(),
            id: id_present,
            key: key_present,
        });
    };
    if !valid_key_id(&key_id) {
        return Err(CredsError::BadKeyId {
            path: path.to_owned(),
        });
    }
    let Some(secret) = decode_secret(&secret_hex) else {
        return Err(CredsError::BadSecret {
            path: path.to_owned(),
        });
    };
    Ok(Creds { key_id, secret })
}

fn read_env_file(path: &Path) -> Result<String, CredsError> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => {
            if meta.permissions().mode() & 0o077 != 0 {
                warn!(
                    "{} is readable by group or others; chmod 600 it. It holds the ideas HMAC secret.",
                    path.display()
                );
            }
        }
        Ok(_) => {
            return Err(CredsError::Io(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(CredsError::Missing {
                path: path.to_owned(),
                id: false,
                key: false,
            });
        }
        Err(err) => {
            return Err(CredsError::Io(format!(
                "could not read {}: {err}",
                path.display()
            )));
        }
    }
    fs::read_to_string(path)
        .map_err(|err| CredsError::Io(format!("could not read {}: {err}", path.display())))
}

/// One `KEY=VALUE` line. `export ` is optional. `#` comments are skipped.
/// Values are not expanded and are not logged.
fn assignment(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = match line.strip_prefix("export") {
        Some(rest) if rest.starts_with(|c: char| c.is_whitespace()) => rest.trim_start(),
        _ => line,
    };
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() || key.chars().any(char::is_whitespace) {
        return None;
    }
    Some((key, unquote(value.trim())))
}

fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let quote = bytes[0];
        if (quote == b'"' || quote == b'\'') && bytes[bytes.len() - 1] == quote {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// Server key ids: `[A-Za-z0-9][A-Za-z0-9._:-]{0,127}`.
fn valid_key_id(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    value.len() <= 128
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// `IDEAS_KEYS` stores 64 hex characters. The HMAC key is the decoded 32 bytes.
fn decode_secret(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn sign(secret: &[u8; 32], ts: &str, idempotency_key: &str, body: &[u8]) -> String {
    let digest = crate::commands::update::apply::hex_lower(&sha256(body));
    let message = format!("{ts}\n{idempotency_key}\n{digest}");
    crate::commands::update::apply::hex_lower(&hmac_sha256(secret, message.as_bytes()))
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed = sha256(key);
        padded[..hashed.len()].copy_from_slice(&hashed);
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0u8; BLOCK];
    let mut opad = [0u8; BLOCK];
    for index in 0..BLOCK {
        ipad[index] = padded[index] ^ 0x36;
        opad[index] = padded[index] ^ 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    idempotency_key: String,
    body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permanent_error: Option<String>,
}

struct Entry {
    path: PathBuf,
    stored: Stored,
}

struct SpoolLock {
    file: File,
}

impl Drop for SpoolLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
    }
}

#[derive(Default)]
struct Report {
    sent: Vec<String>,
    permanent: Vec<String>,
    failed: Vec<String>,
    queued: usize,
    network: Option<String>,
}

impl Report {
    fn emit(self) -> ExitCode {
        for id in &self.sent {
            outln!("{id}");
        }
        for message in self.permanent.iter().chain(&self.failed) {
            error!("{message}");
        }
        if self.queued > 0 {
            let reason = self.network.as_deref().unwrap_or("the network failed");
            error!("{}", queued_line(self.queued, reason));
        }
        if self.permanent.is_empty() && self.failed.is_empty() && self.queued == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn queued_line(count: usize, reason: &str) -> String {
    let ideas = if count == 1 { "idea is" } else { "ideas are" };
    format!("{count} {ideas} queued ({reason}); the next `phux idea` will send them")
}

fn deliver(
    spool: &Path,
    creds: &Creds,
    url: &str,
    new_body: Option<&str>,
    now: impl Fn() -> u64,
) -> Result<Report, String> {
    let _lock = acquire(spool)?;
    if let Some(body) = new_body {
        enqueue(spool, body)?;
    }
    let listed = list_entries(spool)?;
    send_all(spool, &listed, creds, url, now)
}

fn send_all(
    spool: &Path,
    listed: &Listed,
    creds: &Creds,
    url: &str,
    now: impl Fn() -> u64,
) -> Result<Report, String> {
    let mut report = Report {
        failed: listed.problems.clone(),
        ..Report::default()
    };
    for (index, entry) in listed.entries.iter().enumerate() {
        if let Some(message) = preflight(entry)? {
            report.permanent.push(message);
            continue;
        }
        match post_one(
            spool,
            creds,
            url,
            &entry.stored.idempotency_key,
            &entry.stored.body,
            now(),
        ) {
            Ok((status, body)) => apply_response(&mut report, entry, status, &body)?,
            Err(reason) => {
                report.queued = listed.entries[index..]
                    .iter()
                    .filter(|entry| entry.stored.permanent_error.is_none())
                    .count();
                report.network = Some(reason);
                break;
            }
        }
    }
    Ok(report)
}

fn apply_response(
    report: &mut Report,
    entry: &Entry,
    status: u16,
    body: &[u8],
) -> Result<(), String> {
    if (200..300).contains(&status) {
        let Some(id) = capture_id(body) else {
            report.failed.push(format!(
                "ideas capture {} returned HTTP {status} without a capture id; it stays queued",
                entry.stored.idempotency_key
            ));
            return Ok(());
        };
        remove_entry(&entry.path)?;
        report.sent.push(id);
        return Ok(());
    }
    if status == CONFLICT_STATUS {
        mark(entry, CONFLICT_REASON)?;
        report
            .permanent
            .push(conflict_message(&entry.stored.idempotency_key));
        return Ok(());
    }
    report.failed.push(format!(
        "ideas capture {} failed with HTTP {status}; it stays queued",
        entry.stored.idempotency_key
    ));
    Ok(())
}

/// `Some` means do not send. A newly permanent reason is written first.
fn preflight(entry: &Entry) -> Result<Option<String>, String> {
    if let Some(reason) = &entry.stored.permanent_error {
        return Ok(Some(skip_message(&entry.stored.idempotency_key, reason)));
    }
    if entry.stored.body.len() > MAX_BODY_BYTES {
        mark(entry, "too large")?;
        return Ok(Some(skip_message(
            &entry.stored.idempotency_key,
            "too large",
        )));
    }
    if !token_ok(&entry.stored.idempotency_key) {
        mark(entry, "invalid idempotency key")?;
        return Ok(Some(skip_message(
            &entry.stored.idempotency_key,
            "invalid idempotency key",
        )));
    }
    Ok(None)
}

fn conflict_message(idempotency_key: &str) -> String {
    format!(
        "ideas capture {} was rejected with HTTP 409 (idempotency conflict) and will not be retried",
        display_key(idempotency_key)
    )
}

fn skip_message(idempotency_key: &str, reason: &str) -> String {
    if reason == CONFLICT_REASON {
        conflict_message(idempotency_key)
    } else {
        format!(
            "ideas capture {} will not be retried ({reason})",
            display_key(idempotency_key)
        )
    }
}

fn display_key(idempotency_key: &str) -> &str {
    if token_ok(idempotency_key) {
        idempotency_key
    } else {
        "an unreadable spool entry"
    }
}

fn token_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn capture_id(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let id = value.get("id")?.as_str()?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return None;
    }
    Some(id.to_owned())
}

fn post_one(
    spool: &Path,
    creds: &Creds,
    url: &str,
    idempotency_key: &str,
    body: &str,
    now: u64,
) -> Result<(u16, Vec<u8>), String> {
    let ts = now.to_string();
    let signature = sign(&creds.secret, &ts, idempotency_key, body.as_bytes());
    let headers = [
        ("Idempotency-Key", idempotency_key),
        ("x-ideas-key-id", creds.key_id.as_str()),
        ("x-ideas-ts", ts.as_str()),
        ("x-ideas-sig", signature.as_str()),
        ("x-ideas-source", SOURCE_CLI),
    ];
    post(spool, url, &headers, body.as_bytes())
}

fn post(
    spool: &Path,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, Vec<u8>), String> {
    for (name, value) in headers {
        if !token_ok(name) || !token_ok(value) {
            return Err("refusing to send an ideas request with an invalid header".to_owned());
        }
    }
    if url.starts_with("https://") {
        curl(spool, url, "=https", headers, body)
    } else if loopback_http(url) {
        curl(spool, url, "=http", headers, body)
    } else {
        Err(
            "ideas capture only posts to https://ingest.phux.sh, or to a loopback http server in tests"
                .to_owned(),
        )
    }
}

fn loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    host == "127.0.0.1" || host == "localhost"
}

/// Curl invocation plus the spool files it reads and writes.
///
/// The body, the response, and the signature headers live in the ideas
/// spool (mode 0700, held under the spool lock). Each file is created with
/// `O_EXCL` and mode 0600 before curl runs, so a planted name cannot be a
/// symlink curl would follow. Drop removes them.
struct PreparedCurl {
    command: Command,
    response: TempFile,
    body: TempFile,
    headers: TempFile,
}

fn curl(
    spool: &Path,
    url: &str,
    proto: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, Vec<u8>), String> {
    let mut prepared = prepare_curl(spool, url, proto, headers, body)?;
    // Owned by `prepared` until this function returns, so Drop runs after curl.
    let _body = &prepared.body;
    let _headers = &prepared.headers;
    let output = prepared.command.output().map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            "`curl` is not on PATH; ideas capture needs it to reach the server".to_owned()
        } else {
            format!("could not run `curl`: {err}")
        }
    })?;
    let code = String::from_utf8_lossy(&output.stdout);
    let code = code.trim();
    if code.is_empty() || code == "000" || !output.status.success() {
        return Err(curl_failure(&output));
    }
    let status: u16 = code
        .parse()
        .map_err(|_| format!("`curl` returned an unreadable status `{code}`"))?;
    Ok((
        status,
        read_limited(&prepared.response.path, MAX_RESPONSE_BYTES),
    ))
}

fn prepare_curl(
    spool: &Path,
    url: &str,
    proto: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<PreparedCurl, String> {
    // A file, not stdin: curl would otherwise chunk a pipe and the body
    // length would not be the signed bytes. The response file exists before
    // `-o`, so curl opens that regular file.
    let body_file = scratch_file(spool, "body", body)?;
    let response = scratch_file(spool, "http", b"")?;
    let headers_file = scratch_file(spool, "headers", &header_file_bytes(headers))?;
    let mut command = Command::new("curl");
    command
        .arg("-q")
        .arg("--silent")
        .arg("--show-error")
        .arg("--http1.1")
        .arg("--max-time")
        .arg("20")
        .arg("--connect-timeout")
        .arg("5")
        .arg("--proto")
        .arg(proto)
        .arg("--proto-redir")
        .arg("-all")
        .arg("--noproxy")
        .arg("*")
        .arg("-X")
        .arg("POST")
        .arg("-H")
        .arg("Content-Type: text/plain; charset=utf-8")
        .arg("-H")
        .arg("Expect:")
        .arg("-H")
        .arg(format!("@{}", headers_file.path.display()))
        .arg("--data-binary")
        .arg(format!("@{}", body_file.path.display()))
        .arg("-o")
        .arg(&response.path)
        .arg("-w")
        .arg("%{http_code}")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(PreparedCurl {
        command,
        response,
        body: body_file,
        headers: headers_file,
    })
}

fn header_file_bytes(headers: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (name, value) in headers {
        bytes.extend(name.as_bytes());
        bytes.extend(b": ");
        bytes.extend(value.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

fn curl_failure(output: &std::process::Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.lines().next().unwrap_or("").trim();
    let mut end = detail.len().min(180);
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    let detail = &detail[..end];
    if detail.is_empty() {
        format!("curl exited with {}", output.status)
    } else {
        format!("curl exited with {}: {detail}", output.status)
    }
}

fn scratch_file(dir: &Path, suffix: &str, bytes: &[u8]) -> Result<TempFile, String> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!(".phux-idea-{}-{seq}.{suffix}", std::process::id()));
    if let Err(err) = write_private(&path, bytes) {
        let _ = fs::remove_file(&path);
        return Err(err);
    }
    Ok(TempFile { path })
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("could not chmod {}: {err}", path.display()))?;
    file.write_all(bytes)
        .map_err(|err| format!("could not write {}: {err}", path.display()))
}

struct TempFile {
    path: PathBuf,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn read_limited(path: &Path, max: usize) -> Vec<u8> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let mut buf = vec![0; max];
    let read = file.read(&mut buf).unwrap_or(0);
    buf.truncate(read);
    buf
}

struct Listed {
    entries: Vec<Entry>,
    problems: Vec<String>,
}

fn list_entries(spool: &Path) -> Result<Listed, String> {
    let mut names = Vec::new();
    let dir =
        fs::read_dir(spool).map_err(|err| format!("could not read {}: {err}", spool.display()))?;
    for entry in dir {
        let entry = entry.map_err(|err| format!("could not read {}: {err}", spool.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_spool_name(name) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    let mut listed = Listed {
        entries: Vec::new(),
        problems: Vec::new(),
    };
    for name in names {
        match load_entry(&spool.join(&name)) {
            Ok(entry) => listed.entries.push(entry),
            Err(message) => listed.problems.push(message),
        }
    }
    Ok(listed)
}

fn is_spool_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    let parts: Vec<&str> = stem.split('-').collect();
    let widths = [8, 4, 4, 4, 12];
    parts.len() == widths.len()
        && parts
            .iter()
            .zip(widths)
            .all(|(part, width)| part.len() == width && part.chars().all(|c| c.is_ascii_hexdigit()))
}

fn load_entry(path: &Path) -> Result<Entry, String> {
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("idea");
    let meta = fs::symlink_metadata(path)
        .map_err(|err| format!("could not read {label}: {err}; left in place"))?;
    if !meta.file_type().is_file() {
        return Err(format!("{label} is not a regular file; left in place"));
    }
    if meta.len() > MAX_SPOOL_FILE {
        return Err(format!("{label} is too large to read; left in place"));
    }
    let bytes =
        fs::read(path).map_err(|err| format!("could not read {label}: {err}; left in place"))?;
    let stored: Stored = serde_json::from_slice(&bytes)
        .map_err(|_| format!("{label} is not a spooled idea; left in place"))?;
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if stored.idempotency_key != stem {
        return Err(format!(
            "{label} does not match its idempotency key; left in place"
        ));
    }
    Ok(Entry {
        path: path.to_owned(),
        stored,
    })
}

fn enqueue(spool: &Path, body: &str) -> Result<(), String> {
    let idempotency_key = Uuid::now_v7().to_string();
    let path = spool.join(format!("{idempotency_key}.json"));
    let stored = Stored {
        idempotency_key,
        body: body.to_owned(),
        permanent_error: None,
    };
    write_atomic(&path, &entry_bytes(&stored)?)
}

fn mark(entry: &Entry, reason: &str) -> Result<(), String> {
    let stored = Stored {
        idempotency_key: entry.stored.idempotency_key.clone(),
        body: entry.stored.body.clone(),
        permanent_error: Some(reason.to_owned()),
    };
    write_atomic(&entry.path, &entry_bytes(&stored)?)
}

fn entry_bytes(stored: &Stored) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(stored)
        .map_err(|err| format!("could not encode a spooled idea: {err}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn remove_entry(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("could not remove {}: {err}", path.display())),
    }
}

fn acquire(spool: &Path) -> Result<SpoolLock, String> {
    let parent = spool
        .parent()
        .ok_or_else(|| format!("{} has no parent", spool.display()))?;
    ensure_private_dir(parent)?;
    let lock_path = parent.join("spool.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|err| format!("could not open {}: {err}", lock_path.display()))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|err| format!("could not lock {}: {err}", lock_path.display()))?;
    ensure_private_dir(spool)?;
    Ok(SpoolLock { file })
}

fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    if fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(format!(
            "{} is a symlink; refusing to use it for ideas",
            dir.display()
        ));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    let tmp = hidden_tmp(path);
    let _ = fs::remove_file(&tmp);
    let write = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| format!("could not create {}: {err}", tmp.display()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
        fs::rename(&tmp, path)
            .map_err(|err| format!("could not replace {}: {err}", path.display()))?;
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|err| format!("could not sync {}: {err}", parent.display()))
    })();
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write
}

fn hidden_tmp(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
