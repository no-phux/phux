//! The `opencode` service: the host's `OpenCode` background service, reached
//! on loopback with a session token this server mints for itself.
//!
//! Only two CLI outputs are read, both typed contracts: `opencode service
//! status` prints exactly the service origin (anything else means it is not
//! running), and `opencode api server.pair` prints the pairing JSON. The
//! code is exchanged at `GET {origin}/auth/connect/{code}` for a session
//! token used as the Basic password of user `opencode`. Origin and token are
//! cached for the process; a connect failure rediscovers the origin and a
//! `401` re-mints the token (see `super::upstream`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use base64::Engine as _;
use http::header::{self, HeaderValue};
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Mutex;

use super::Refusal;

const STATUS_DEADLINE: Duration = Duration::from_secs(5);
const START_DEADLINE: Duration = Duration::from_secs(20);
const PAIR_DEADLINE: Duration = Duration::from_secs(10);
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
/// Most stdout read from one CLI call; both contracts are a few dozen bytes.
const MAX_CLI_OUTPUT: u64 = 64 * 1024;
/// Most bytes of the `/auth/connect` reply read.
const MAX_CONNECT_REPLY: usize = 64 * 1024;

/// The service's loopback address, as `service status` printed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Origin(SocketAddr);

impl Origin {
    pub(super) const fn addr(self) -> SocketAddr {
        self.0
    }

    /// The `Host` header value: `127.0.0.1:PORT` or `[::1]:PORT`.
    pub(super) fn authority(self) -> String {
        self.0.to_string()
    }
}

/// A session token; never printed.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Token(String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

/// What one request authenticates with: the origin, its `Host`, and the
/// token as `Authorization: Basic base64(opencode:TOKEN)`.
#[derive(Debug, Clone)]
pub(super) struct Credentials {
    pub(super) origin: Origin,
    pub(super) token: Token,
    pub(super) host: HeaderValue,
    pub(super) authorization: HeaderValue,
}

impl Credentials {
    fn new(origin: Origin, token: Token) -> Result<Self, Refusal> {
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("opencode:{}", token.0));
        let mut authorization = header_value(&format!("Basic {basic}"))?;
        authorization.set_sensitive(true);
        Ok(Self {
            host: header_value(&origin.authority())?,
            origin,
            token,
            authorization,
        })
    }
}

fn header_value(value: &str) -> Result<HeaderValue, Refusal> {
    HeaderValue::from_str(value).map_err(|err| Refusal::internal(format!("header value: {err}")))
}

#[derive(Debug, Default)]
struct Cache {
    origin: Option<Origin>,
    token: Option<Token>,
}

/// The `OpenCode` provider. One per process: its cache is the host's service.
#[derive(Debug)]
pub(super) struct OpenCode {
    /// `None` resolves the installed executable on each discovery, so an
    /// install after the server started is found.
    executable: Option<PathBuf>,
    cache: Mutex<Cache>,
}

impl OpenCode {
    /// The host's installed `OpenCode`.
    pub(super) const fn installed() -> Self {
        Self {
            executable: None,
            cache: Mutex::const_new(Cache {
                origin: None,
                token: None,
            }),
        }
    }

    /// `OpenCode` at a fixed path (tests run a fake CLI).
    #[cfg(test)]
    pub(super) fn at(executable: PathBuf) -> Self {
        Self {
            executable: Some(executable),
            cache: Mutex::new(Cache::default()),
        }
    }

    /// The cached origin and token, discovering and minting what is missing.
    /// Concurrent callers wait for one discovery rather than each starting
    /// the service.
    pub(super) async fn credentials(&self) -> Result<Credentials, Refusal> {
        let mut cache = self.cache.lock().await;
        let executable = self.executable()?;
        let origin = match cache.origin {
            Some(origin) => origin,
            None => *cache.origin.insert(discover(&executable).await?),
        };
        let token = match &cache.token {
            Some(token) => token.clone(),
            None => cache.token.insert(mint(&executable, origin).await?).clone(),
        };
        drop(cache);
        Credentials::new(origin, token)
    }

    /// Drop `stale` so the next [`Self::credentials`] rediscovers; a newer
    /// origin another caller found is kept.
    pub(super) async fn forget_origin(&self, stale: Origin) {
        let mut cache = self.cache.lock().await;
        if cache.origin == Some(stale) {
            cache.origin = None;
        }
    }

    /// Drop `stale` so the next [`Self::credentials`] mints a new token; a
    /// newer token another caller minted is kept.
    pub(super) async fn forget_token(&self, stale: &Token) {
        let mut cache = self.cache.lock().await;
        if cache.token.as_ref() == Some(stale) {
            cache.token = None;
        }
    }

    fn executable(&self) -> Result<PathBuf, Refusal> {
        self.executable
            .clone()
            .or_else(phux_plugin::opencode_executable_path)
            .ok_or_else(|| {
                Refusal::unavailable(
                    "OpenCode is not installed on this host: no `opencode` on PATH \
                     or at ~/.opencode/bin/opencode",
                )
            })
    }
}

/// The running service's origin, starting the service once if it is not.
async fn discover(executable: &Path) -> Result<Origin, Refusal> {
    if let Some(origin) = status(executable).await? {
        return Ok(origin);
    }
    run_quiet(executable, &["service", "start"], START_DEADLINE).await?;
    status(executable).await?.ok_or_else(|| {
        Refusal::unavailable(
            "OpenCode service did not start: `opencode service status` reports no \
             loopback origin after `opencode service start`",
        )
    })
}

async fn status(executable: &Path) -> Result<Option<Origin>, Refusal> {
    let stdout = run_output(executable, &["service", "status"], STATUS_DEADLINE).await?;
    Ok(parse_origin(&stdout))
}

/// `http://127.0.0.1:PORT` or `http://[::1]:PORT` on one line; anything
/// else (`stopped`, a remote URL, extra lines) is no usable origin.
pub(super) fn parse_origin(stdout: &[u8]) -> Option<Origin> {
    let line = std::str::from_utf8(stdout).ok()?.trim();
    let authority = line.strip_prefix("http://")?;
    let addr: SocketAddr = authority.parse().ok()?;
    let loopback = matches!(
        addr.ip(),
        IpAddr::V4(Ipv4Addr::LOCALHOST) | IpAddr::V6(Ipv6Addr::LOCALHOST)
    );
    (loopback && addr.port() != 0).then_some(Origin(addr))
}

#[derive(Deserialize)]
struct Pairing {
    code: String,
}

#[derive(Deserialize)]
struct Session {
    token: String,
}

/// A fresh session token: a pairing code from the CLI, exchanged on
/// loopback.
async fn mint(executable: &Path, origin: Origin) -> Result<Token, Refusal> {
    let stdout = run_output(executable, &["api", "server.pair"], PAIR_DEADLINE).await?;
    let pairing: Pairing = serde_json::from_slice(&stdout).map_err(|_| {
        Refusal::unavailable("`opencode api server.pair` did not print its pairing JSON")
    })?;
    if !is_url_safe(&pairing.code) {
        return Err(Refusal::unavailable(
            "`opencode api server.pair` printed a malformed pairing code",
        ));
    }
    let token = exchange_code(origin, &pairing.code).await?;
    if token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(Refusal::unavailable(
            "OpenCode answered the pairing code with a malformed token",
        ));
    }
    Ok(Token(token))
}

fn is_url_safe(code: &str) -> bool {
    !code.is_empty()
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

async fn exchange_code(origin: Origin, code: &str) -> Result<String, Refusal> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("/auth/connect/{code}"))
        .header(header::HOST, origin.authority())
        .header(header::ACCEPT, "application/json")
        .body(Full::default())
        .map_err(|err| Refusal::internal(format!("pairing request: {err}")))?;
    let response = tokio::time::timeout(
        CONNECT_DEADLINE,
        super::upstream::send(origin.addr(), request),
    )
    .await
    .map_err(|_| Refusal::unavailable("OpenCode did not answer the pairing exchange in time"))?
    .map_err(|err| Refusal::unavailable(format!("OpenCode pairing exchange failed: {err}")))?;
    if response.status() != StatusCode::OK {
        return Err(Refusal::unavailable(format!(
            "OpenCode refused the pairing code ({})",
            response.status()
        )));
    }
    let body = Limited::new(response.into_body(), MAX_CONNECT_REPLY)
        .collect()
        .await
        .map_err(|err| Refusal::unavailable(format!("OpenCode pairing reply: {err}")))?
        .to_bytes();
    let session: Session = serde_json::from_slice(&body)
        .map_err(|_| Refusal::unavailable("OpenCode's pairing reply carried no token"))?;
    Ok(session.token)
}

fn cli(executable: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

/// Run a CLI call to success, reading none of its output.
async fn run_quiet(executable: &Path, args: &[&str], deadline: Duration) -> Result<(), Refusal> {
    let mut command = cli(executable, args);
    command.stdout(Stdio::null());
    let label = args.join(" ");
    let mut child = command.spawn().map_err(|err| spawn_failed(&label, &err))?;
    let status = tokio::time::timeout(deadline, child.wait())
        .await
        .map_err(|_| timed_out(&label, deadline))?
        .map_err(|err| spawn_failed(&label, &err))?;
    succeeded(&label, status)
}

/// Run a CLI call to success and return its bounded stdout.
async fn run_output(
    executable: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<Vec<u8>, Refusal> {
    let mut command = cli(executable, args);
    command.stdout(Stdio::piped());
    let label = args.join(" ");
    let mut child = command.spawn().map_err(|err| spawn_failed(&label, &err))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Refusal::internal(format!("`opencode {label}` has no stdout pipe")))?;
    let run = async {
        let mut output = Vec::new();
        (&mut stdout)
            .take(MAX_CLI_OUTPUT + 1)
            .read_to_end(&mut output)
            .await?;
        Ok::<_, std::io::Error>((child.wait().await?, output))
    };
    let (status, output) = tokio::time::timeout(deadline, run)
        .await
        .map_err(|_| timed_out(&label, deadline))?
        .map_err(|err| spawn_failed(&label, &err))?;
    succeeded(&label, status)?;
    if output.len() as u64 > MAX_CLI_OUTPUT {
        return Err(Refusal::unavailable(format!(
            "`opencode {label}` printed more than {MAX_CLI_OUTPUT} bytes"
        )));
    }
    Ok(output)
}

fn succeeded(label: &str, status: std::process::ExitStatus) -> Result<(), Refusal> {
    if status.success() {
        return Ok(());
    }
    Err(Refusal::unavailable(format!(
        "`opencode {label}` failed ({status})"
    )))
}

fn spawn_failed(label: &str, err: &std::io::Error) -> Refusal {
    Refusal::unavailable(format!("could not run `opencode {label}`: {err}"))
}

fn timed_out(label: &str, deadline: Duration) -> Refusal {
    Refusal::unavailable(format!(
        "`opencode {label}` did not finish within {}s",
        deadline.as_secs()
    ))
}
