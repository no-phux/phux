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
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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
pub(crate) fn run(private: bool, flush: bool, text: Option<&str>) -> ExitCode {
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
    let digest = hex_encode(&sha256(body));
    let message = format!("{ts}\n{idempotency_key}\n{digest}");
    hex_encode(&hmac_sha256(secret, message.as_bytes()))
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
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
    let digest = outer.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
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
    send_all(&listed, creds, url, now)
}

fn send_all(
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
    post(url, &headers, body.as_bytes())
}

fn post(url: &str, headers: &[(&str, &str)], body: &[u8]) -> Result<(u16, Vec<u8>), String> {
    for (name, value) in headers {
        if !token_ok(name) || !token_ok(value) {
            return Err("refusing to send an ideas request with an invalid header".to_owned());
        }
    }
    if url.starts_with("https://") {
        curl(url, "=https", headers, body)
    } else if loopback_http(url) {
        curl(url, "=http", headers, body)
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

fn curl(
    url: &str,
    proto: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, Vec<u8>), String> {
    // A file, not stdin: curl would otherwise chunk a pipe and the body
    // length would not be the signed bytes.
    let body_path = temp_path("body");
    let out = temp_path("http");
    let _body_guard = TempFile {
        path: body_path.clone(),
    };
    let _out_guard = TempFile { path: out.clone() };
    write_private(&body_path, body)?;
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
        .arg("--data-binary")
        .arg(format!("@{}", body_path.display()))
        .arg("-o")
        .arg(&out)
        .arg("-w")
        .arg("%{http_code}");
    for (name, value) in headers {
        command.arg("-H").arg(format!("{name}: {value}"));
    }
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output().map_err(|err| {
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
    Ok((status, read_limited(&out, MAX_RESPONSE_BYTES)))
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

fn temp_path(suffix: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("phux-idea-{}-{seq}.{suffix}", std::process::id()))
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
    fs::create_dir_all(dir).map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("could not chmod {}: {err}", dir.display()))
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
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use super::*;

    const SECRET_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const VECTOR_TS: &str = "1700000000";
    const VECTOR_IDEM: &str = "018f6b3c-7c3a-7b2e-8c1d-4e5f60718293";
    const VECTOR_BODY: &str = "ship the idea spool";
    /// `openssl dgst -sha256 -mac HMAC -macopt hexkey:$SECRET_HEX` over
    /// `ts \\n idem \\n sha256hex(body)`. Not computed by [`sign`].
    const VECTOR_SIG: &str = "2e300946913db246e3d836ef23cc95d400b7122944b6e179dba59cad914c68da";

    struct Hit {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    struct Mock {
        addr: std::net::SocketAddr,
        hits: Arc<Mutex<Vec<Hit>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Mock {
        fn spawn(status: u16, reason: &'static str, body: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let hits = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let hits_thread = Arc::clone(&hits);
            let stop_thread = Arc::clone(&stop);
            let thread = thread::spawn(move || {
                while !stop_thread.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    if stop_thread.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Some(hit) = read_request(&mut stream) {
                        hits_thread.lock().expect("hits").push(hit);
                        let bytes = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(bytes.as_bytes());
                    }
                }
            });
            Self {
                addr,
                hits,
                stop,
                thread: Some(thread),
            }
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/ideas/capture", self.addr.port())
        }

        fn hits(&self) -> Vec<Hit> {
            self.hits.lock().expect("hits").drain(..).collect()
        }
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn read_request(stream: &mut TcpStream) -> Option<Hit> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos;
            }
            if buf.len() > 64 * 1024 {
                return None;
            }
            match stream.read(&mut tmp) {
                Ok(0) | Err(_) => return None,
                Ok(read) => buf.extend_from_slice(&tmp[..read]),
            }
        };
        let head = std::str::from_utf8(&buf[..header_end]).ok()?;
        let mut lines = head.split("\r\n");
        let request = lines.next()?;
        let mut parts = request.split_whitespace();
        let method = parts.next()?.to_owned();
        let path = parts.next()?.to_owned();
        let mut headers = Vec::new();
        let mut content_length = 0usize;
        for line in lines {
            let (name, value) = line.split_once(':')?;
            let name = name.trim().to_owned();
            let value = value.trim().to_owned();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().ok()?;
            }
            headers.push((name, value));
        }
        let mut body = buf[header_end + 4..].to_vec();
        while body.len() < content_length {
            match stream.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(read) => body.extend_from_slice(&tmp[..read]),
            }
        }
        body.truncate(content_length);
        Some(Hit {
            method,
            path,
            headers,
            body,
        })
    }

    fn header<'a>(hit: &'a Hit, name: &str) -> &'a str {
        hit.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map_or("", |(_, value)| value.as_str())
    }

    fn write_env(path: &Path, body: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .expect("ops.env");
        file.write_all(body.as_bytes()).expect("write");
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .expect("chmod");
    }

    fn creds(dir: &Path) -> Creds {
        let path = dir.join("ops.env");
        write_env(
            &path,
            &format!(
                "# ideas capture\nexport PHUX_IDEAS_KEY_ID=\"cli-test\"\nPHUX_IDEAS_KEY='{SECRET_HEX}'\nPHUX_OPS_KEY=ignored\n"
            ),
        );
        load_creds(&path).expect("creds")
    }

    fn closed_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        format!("http://127.0.0.1:{port}/ideas/capture")
    }

    fn spool_files(spool: &Path) -> Vec<PathBuf> {
        let mut files: Vec<_> = fs::read_dir(spool)
            .expect("spool")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort();
        files
    }

    #[test]
    fn signature_on_the_wire_matches_the_known_vector() {
        let root = tempfile::tempdir().expect("temp");
        let creds = creds(root.path());
        assert!(!format!("{creds:?}").contains(SECRET_HEX));
        let spool = root.path().join("spool");
        fs::create_dir_all(&spool).expect("spool");
        let stored = Stored {
            idempotency_key: VECTOR_IDEM.to_owned(),
            body: VECTOR_BODY.to_owned(),
            permanent_error: None,
        };
        write_atomic(
            &spool.join(format!("{VECTOR_IDEM}.json")),
            &entry_bytes(&stored).expect("json"),
        )
        .expect("seed");
        let mock = Mock::spawn(201, "Created", "{\"id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\"}");
        let report = deliver(&spool, &creds, &mock.url(), None, || {
            VECTOR_TS.parse().expect("ts")
        })
        .expect("deliver");
        assert_eq!(report.sent, vec!["01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()]);
        assert!(report.permanent.is_empty());
        assert_eq!(report.queued, 0);
        assert!(spool_files(&spool).is_empty());
        let hits = mock.hits();
        assert_eq!(hits.len(), 1, "one post");
        let hit = &hits[0];
        assert_eq!(hit.method, "POST");
        assert_eq!(hit.path, "/ideas/capture");
        assert_eq!(hit.body, VECTOR_BODY.as_bytes());
        assert_eq!(header(hit, "Idempotency-Key"), VECTOR_IDEM);
        assert_eq!(header(hit, "x-ideas-key-id"), "cli-test");
        assert_eq!(header(hit, "x-ideas-ts"), VECTOR_TS);
        assert_eq!(header(hit, "x-ideas-sig"), VECTOR_SIG);
        assert_eq!(header(hit, "x-ideas-source"), "cli");
        assert!(
            !hit.headers
                .iter()
                .any(|(_, value)| value.contains(SECRET_HEX))
        );
    }

    #[test]
    fn offline_capture_replays_exactly_once_when_the_network_returns() {
        let root = tempfile::tempdir().expect("temp");
        let creds = creds(root.path());
        let spool = root.path().join("ideas").join("spool");
        let body = "remember the milk";
        let queued = deliver(&spool, &creds, &closed_url(), Some(body), unix_now).expect("queue");
        assert!(queued.sent.is_empty());
        assert_eq!(queued.queued, 1);
        let files = spool_files(&spool);
        assert_eq!(files.len(), 1);
        let mode = fs::metadata(&files[0]).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let entry = load_entry(&files[0]).expect("entry");
        assert_eq!(entry.stored.body, body);
        let parsed = Uuid::parse_str(&entry.stored.idempotency_key).expect("uuid");
        assert_eq!(parsed.get_version(), Some(uuid::Version::SortRand));
        let key = entry.stored.idempotency_key;

        let mock = Mock::spawn(200, "OK", "{\"id\":\"01REPLAYCAPTUREID000000000\"}");
        let sent = deliver(&spool, &creds, &mock.url(), None, || 1_700_000_100).expect("replay");
        assert_eq!(sent.sent, vec!["01REPLAYCAPTUREID000000000".to_owned()]);
        assert_eq!(sent.queued, 0);
        assert!(spool_files(&spool).is_empty());
        let hits = mock.hits();
        assert_eq!(hits.len(), 1, "the queued idea is posted once");
        assert_eq!(header(&hits[0], "Idempotency-Key"), key);
        assert_eq!(hits[0].body, body.as_bytes());
        assert_eq!(header(&hits[0], "x-ideas-ts"), "1700000100");
        assert_eq!(header(&hits[0], "x-ideas-sig").len(), 64);

        let again = deliver(&spool, &creds, &mock.url(), None, || 1_700_000_200).expect("empty");
        assert!(again.sent.is_empty());
        assert_eq!(again.queued, 0);
        assert!(
            mock.hits().is_empty(),
            "a flushed spool does not send again"
        );
    }

    #[test]
    fn idempotency_conflict_is_not_retried() {
        let root = tempfile::tempdir().expect("temp");
        let creds = creds(root.path());
        let spool = root.path().join("spool");
        fs::create_dir_all(&spool).expect("spool");
        let stored = Stored {
            idempotency_key: VECTOR_IDEM.to_owned(),
            body: "different body".to_owned(),
            permanent_error: None,
        };
        write_atomic(
            &spool.join(format!("{VECTOR_IDEM}.json")),
            &entry_bytes(&stored).expect("json"),
        )
        .expect("seed");
        let mock = Mock::spawn(409, "Conflict", "{\"error\":\"idempotency conflict\"}");
        let first = deliver(&spool, &creds, &mock.url(), None, unix_now).expect("conflict");
        assert!(first.sent.is_empty());
        assert_eq!(first.permanent.len(), 1);
        assert!(first.permanent[0].contains("409"));
        assert!(first.permanent[0].contains("will not be retried"));
        assert!(!first.permanent[0].contains("different body"));
        let entry = load_entry(&spool.join(format!("{VECTOR_IDEM}.json"))).expect("kept");
        assert_eq!(
            entry.stored.permanent_error.as_deref(),
            Some(CONFLICT_REASON)
        );
        assert_eq!(mock.hits().len(), 1);

        let second = deliver(&spool, &creds, &mock.url(), None, unix_now).expect("skip");
        assert!(second.sent.is_empty());
        assert_eq!(second.permanent.len(), 1);
        assert!(
            mock.hits().is_empty(),
            "a 409 stays in the spool and is not posted again"
        );
    }

    #[test]
    fn private_tag_is_ensured_and_oversized_ideas_are_refused() {
        assert_eq!(
            prepare_body("keep this", true).expect("private"),
            "#private\nkeep this"
        );
        assert_eq!(
            prepare_body("already #private here", true).expect("present"),
            "already #private here"
        );
        assert_eq!(prepare_body("", true).expect("tag only"), "#private");
        assert!(prepare_body("", false).is_err());
        let max = "a".repeat(MAX_BODY_BYTES);
        assert_eq!(
            prepare_body(&max, false).expect("limit").len(),
            MAX_BODY_BYTES
        );
        assert!(prepare_body(&format!("{max}b"), false).is_err());
        assert!(prepare_body(&max, true).is_err());
    }

    #[test]
    fn a_bad_secret_is_not_printed() {
        let root = tempfile::tempdir().expect("temp");
        let path = root.path().join("ops.env");
        let sentinel = "z".repeat(64);
        write_env(
            &path,
            &format!("PHUX_IDEAS_KEY_ID=cli-test\nPHUX_IDEAS_KEY={sentinel}\n"),
        );
        let err = load_creds(&path).expect_err("bad secret");
        let rendered = err.to_string();
        assert!(rendered.contains("64 hex"));
        assert!(rendered.contains("not printed"));
        assert!(!rendered.contains(&sentinel));
        assert!(!format!("{err:?}").contains(&sentinel));
    }

    #[test]
    fn missing_keys_name_the_file_and_not_a_value() {
        let root = tempfile::tempdir().expect("temp");
        let path = root.path().join("ops.env");
        let err = load_creds(&path).expect_err("missing file");
        assert!(err.to_string().contains("PHUX_IDEAS_KEY_ID"));
        assert!(err.to_string().contains("PHUX_IDEAS_KEY"));
        assert!(err.to_string().contains(path.to_str().expect("utf8")));
        write_env(&path, "PHUX_IDEAS_KEY_ID=cli-test\n");
        let err = load_creds(&path).expect_err("missing secret");
        assert!(err.to_string().contains("PHUX_IDEAS_KEY"));
        assert!(!err.to_string().contains("PHUX_IDEAS_KEY_ID and"));
    }

    #[test]
    fn idea_command_parses_and_does_not_take_the_server_socket() {
        let cli = crate::parse_cli(["phux", "idea", "--private", "a note"]).expect("parse");
        let command = cli.command.expect("verb");
        assert!(matches!(
            command,
            crate::commands::Command::Idea {
                private: true,
                flush: false,
                ref text,
            } if text.as_deref() == Some("a note")
        ));
        assert_eq!(crate::commands::socketless_verb(&command), Some("idea"));
        let flush = crate::parse_cli(["phux", "idea", "--flush"]).expect("flush");
        assert!(matches!(
            flush.command,
            Some(crate::commands::Command::Idea {
                flush: true,
                text: None,
                ..
            })
        ));
    }

    #[test]
    fn capture_posts_to_the_ingest_endpoint() {
        assert_eq!(CAPTURE_URL, "https://ingest.phux.sh/ideas/capture");
    }
}
