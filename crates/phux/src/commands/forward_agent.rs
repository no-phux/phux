//! `phux forward --agent`: serve this machine's ssh-agent to the panes on
//! a phux server (`docs/ssh.md#ssh-agent`). Runs until Ctrl-C.
//!
//! The server sends each pane request as an event; this answers it from
//! the local agent at `SSH_AUTH_SOCK`. It trusts the server for nothing:
//! only a key listing and a signature are ever passed on, and a signature
//! only after the user says yes to it here (or ran with `--yes`).

use std::io::{BufRead as _, IsTerminal as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_client_runtime::control::forward::{self, ForwardError};
use phux_client_runtime::control::{Event, Status};
use phux_client_runtime::runtime::Client;
use phux_protocol::forward::{
    AGENT_REQUEST_SECS, MAX_AGENT_MESSAGE_BYTES, SSH_AGENT_FAILURE, SSH_AGENT_IDENTITIES_ANSWER,
    SSH_AGENTC_REQUEST_IDENTITIES, SSH_AGENTC_SIGN_REQUEST, sign_request_key,
};
use phux_protocol::wire::frame::ErrorCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::forward::{Activity, unreachable_server};

/// How long the server waits for an answer; past it a signature is wasted.
const DEADLINE: Duration = Duration::from_secs(AGENT_REQUEST_SECS);

/// How a signature gets its yes.
enum Consent {
    /// `--yes`: sign without asking.
    Always,
    /// Nobody to ask (stdin is not a terminal): never sign.
    Never,
    /// Show the question, return the answer. One question at a time.
    Ask(Arc<dyn Fn(&str) -> bool + Send + Sync>),
}

/// What every request is answered with.
struct Serving {
    /// The local agent's socket.
    agent: PathBuf,
    /// Who is asking, for the prompt.
    host: String,
    consent: Consent,
    /// Serializes the questions.
    asking: tokio::sync::Mutex<()>,
}

/// One request and its answer from the local agent at `socket`.
async fn local(socket: &Path, message: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut stream = UnixStream::connect(socket).await?;
    let len = u32::try_from(message.len()).map_err(std::io::Error::other)?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(message).await?;
    let len = usize::try_from(stream.read_u32().await?).map_err(std::io::Error::other)?;
    if len > MAX_AGENT_MESSAGE_BYTES {
        return Err(std::io::Error::other("the agent's answer is too long"));
    }
    let mut answer = vec![0; len];
    stream.read_exact(&mut answer).await?;
    Ok(answer)
}

/// The comment the local agent holds `key` under; `None` when it does not
/// hold that key.
fn comment_of(identities: &[u8], key: &[u8]) -> Option<String> {
    fn string<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
        let (len, tail) = rest.split_first_chunk::<4>()?;
        let (value, tail) = tail.split_at_checked(u32::from_be_bytes(*len) as usize)?;
        *rest = tail;
        Some(value)
    }
    let mut rest = identities.strip_prefix(&[SSH_AGENT_IDENTITIES_ANSWER])?;
    let (count, tail) = rest.split_first_chunk::<4>()?;
    rest = tail;
    for _ in 0..u32::from_be_bytes(*count) {
        let (blob, comment) = (string(&mut rest)?, string(&mut rest)?);
        if blob == key {
            return Some(String::from_utf8_lossy(comment).into_owned());
        }
    }
    None
}

/// A key's fingerprint as `ssh-add -l` prints it.
fn fingerprint(key: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(key);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// Only printable ASCII of a key comment reaches the terminal.
fn printable(comment: &str) -> String {
    comment
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(80)
        .collect()
}

impl Serving {
    /// The ssh-agent response to `message`, received at `started`.
    /// Anything refused, failed, or late is `SSH_AGENT_FAILURE`.
    async fn answer(&self, message: &[u8], started: Instant) -> Vec<u8> {
        let passed = match message.first() {
            Some(&SSH_AGENTC_REQUEST_IDENTITIES) => true,
            Some(&SSH_AGENTC_SIGN_REQUEST) => self.may_sign(message, started).await,
            // The server already refuses the rest; so does this.
            _ => false,
        };
        if passed && let Ok(answer) = local(&self.agent, message).await {
            return answer;
        }
        vec![SSH_AGENT_FAILURE]
    }

    /// Whether the sign request `message` goes to the local agent.
    async fn may_sign(&self, message: &[u8], started: Instant) -> bool {
        let Some(key) = sign_request_key(message) else {
            return false;
        };
        let print = fingerprint(key);
        let listing = local(&self.agent, &[SSH_AGENTC_REQUEST_IDENTITIES]).await;
        let Some(comment) = listing.ok().and_then(|list| comment_of(&list, key)) else {
            warn!(
                "{}: refused a signature with {print}, a key this agent does not hold",
                self.host
            );
            return false;
        };
        let key = format!("{print} ({})", printable(&comment));
        let ask = match &self.consent {
            Consent::Always => {
                info!("{}: signing with {key}", self.host);
                return true;
            }
            Consent::Never => {
                warn!(
                    "{}: refused a signature with {key}: stdin is not a terminal, so there is \
                     no one to ask (--yes signs without asking)",
                    self.host
                );
                return false;
            }
            Consent::Ask(ask) => Arc::clone(ask),
        };
        let _one_at_a_time = self.asking.lock().await;
        if started.elapsed() >= DEADLINE {
            warn!(
                "{}: a request to sign with {key} expired unanswered",
                self.host
            );
            return false;
        }
        let question = format!("phux: let {} sign with {key}? [y/N] ", self.host);
        // Off the runtime's thread, so Ctrl-C still ends the command while
        // the question stands.
        let said_yes = tokio::task::spawn_blocking(move || ask(&question)).await;
        if !said_yes.unwrap_or(false) {
            info!("refused");
            return false;
        }
        if started.elapsed() >= DEADLINE {
            warn!(
                "too late: the server stopped waiting after {AGENT_REQUEST_SECS} s; nothing was signed"
            );
            return false;
        }
        true
    }
}

/// Ask on stderr, read the answer from stdin.
fn ask_terminal(question: &str) -> bool {
    eprint!("{question}");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

async fn offer(client: &Client) -> Result<(), ForwardError> {
    client
        .with_control(phux_client_runtime::control::ControlPlane::agent_offer)?
        .await
}

/// Answer request `id` and send the answer back.
async fn handle(client: Arc<Client>, serving: Arc<Serving>, id: u64, message: Vec<u8>) {
    let answer = serving.answer(&message, Instant::now()).await;
    let sent = match client.with_control(|control| control.agent_reply(id, answer)) {
        Ok(call) => call.await,
        Err(err) => Err(err),
    };
    if let Err(ForwardError::Refused {
        code: ErrorCode::NotFound,
        ..
    }) = sent
    {
        warn!(
            "{}: the server had stopped waiting for that answer",
            serving.host
        );
    }
}

pub(super) async fn serve(
    client: &Arc<Client>,
    activity: &Activity,
    host: Option<String>,
    yes: bool,
) -> ExitCode {
    if !client.with_control(|control| control.forward_agent_supported()) {
        error!(
            "the server does not advertise FORWARD_AGENT: upgrade phux on that host, and set \
             `[forward] agent = true` in its config"
        );
        return ExitCode::FAILURE;
    }
    let Some(agent) = std::env::var_os("SSH_AUTH_SOCK").filter(|path| !path.is_empty()) else {
        error!("SSH_AUTH_SOCK is not set: there is no ssh-agent here to serve");
        return ExitCode::FAILURE;
    };
    let (consent, how) = if yes {
        (Consent::Always, "signing without asking")
    } else if std::io::stdin().is_terminal() {
        let ask = Consent::Ask(Arc::new(ask_terminal));
        (ask, "each signature is asked here")
    } else {
        let how = "listing keys only: stdin is not a terminal, so every signature is refused \
                   (--yes signs without asking)";
        (Consent::Never, how)
    };
    let serving = Arc::new(Serving {
        agent: agent.into(),
        host: host.unwrap_or_else(|| "the local phux server".to_owned()),
        consent,
        asking: tokio::sync::Mutex::new(()),
    });
    if let Err(err) = offer(client).await {
        error!("the server refused the offer: {err}");
        return ExitCode::FAILURE;
    }
    outln!(
        "Serving your ssh-agent to panes on {}; {how}; Ctrl-C stops",
        serving.host
    );
    let mut lost = false;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
            () = activity.0.notified() => {}
        }
        for event in client.take_events() {
            match event {
                Event::Frame(_) => {
                    if let Some((id, message)) = forward::agent_request(event) {
                        tokio::spawn(handle(
                            Arc::clone(client),
                            Arc::clone(&serving),
                            id,
                            message,
                        ));
                    }
                }
                // Once per outage: every failed redial reports again.
                Event::ConnectionLost { .. } if !lost => {
                    lost = true;
                    warn!("lost the phux connection; panes get no agent until it is back");
                }
                // A new connection carries no offer: make it again.
                Event::StatusChanged(Status::Attached) if lost => {
                    lost = false;
                    match offer(client).await {
                        Ok(()) => info!("reconnected; serving again"),
                        Err(err) => {
                            error!("reconnected, but the server refused the offer: {err}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                _ => {}
            }
        }
        if client.status().is_terminal() {
            return unreachable_server(client);
        }
    }
}

#[cfg(test)]
#[path = "forward_agent_tests.rs"]
mod tests;
