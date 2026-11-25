//! Serving this machine's ssh-agent to a phux server's panes
//! (`docs/ssh.md#ssh-agent`): the one policy behind `phux forward --agent`
//! and every client that serves while attached.
//!
//! The server sends each pane request as an event
//! ([`forward::agent_request`](crate::control::forward::agent_request)); an
//! [`AgentServing`] answers it from the local agent. It trusts the server for
//! nothing: only a key listing and a signature are ever passed on, only for
//! the allowed keys, and a signature only after the user said yes to it here.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use phux_protocol::ResourceId;
use phux_protocol::forward::{
    AGENT_REQUEST_SECS, MAX_AGENT_MESSAGE_BYTES, SSH_AGENT_FAILURE, SSH_AGENT_IDENTITIES_ANSWER,
    SSH_AGENTC_REQUEST_IDENTITIES, SSH_AGENTC_SIGN_REQUEST, sign_request_key,
};

pub use crate::control::forward::{AgentRequest, request_in};

/// How long the server waits for an answer; past it a signature is wasted.
const DEADLINE: Duration = Duration::from_secs(AGENT_REQUEST_SECS);

/// Requests answered at once, each on its own thread; one more is refused.
/// The server holds no more than this, but nothing here relies on that.
const MAX_IN_FLIGHT: usize = 64;

/// How long a new question ignores whatever would sign: a word being typed,
/// or a Return already on its way, must not answer a question nobody saw.
pub const QUESTION_ARMING: Duration = Duration::from_millis(500);

/// One signature waiting for a person's yes. Dropped unanswered, it is a no.
#[derive(Debug)]
pub struct Question {
    /// The host asking, as the user named it.
    pub host: String,
    /// The pane asking, as the server guessed it: a hint, since any process
    /// of the server's user can look like a pane's. `None` when the server
    /// could not tell (or is too old to say).
    pub pane: Option<ResourceId>,
    /// The key: its fingerprint, and its comment as the local agent has it.
    pub key: String,
    /// When the server stops waiting; a later yes signs nothing.
    pub expires: Instant,
    answer: mpsc::SyncSender<bool>,
}

impl Question {
    /// Who is asking. `label` is what the client calls the pane, which may be
    /// the remote's own words: only its printable ASCII is shown.
    #[must_use]
    pub fn asker(&self, label: Option<&str>) -> String {
        let Some(pane) = &self.pane else {
            return format!("a process on {} (pane unknown)", self.host);
        };
        let called = label
            .map(printable)
            .filter(|label| !label.is_empty())
            .map(|label| format!(" ({label})"))
            .unwrap_or_default();
        format!("pane {pane}{called} on {}", self.host)
    }

    /// Answer it.
    pub fn answer(&self, yes: bool) {
        // The wait may have ended; the answer then goes nowhere.
        let _ = self.answer.try_send(yes);
    }
}

/// How a signature gets its yes.
pub enum Consent {
    /// Sign without asking.
    Always,
    /// Nobody to ask: never sign.
    Never,
    /// Show the question; [`Question::answer`] ends it. Called on the
    /// request's own thread, one question at a time.
    Ask(Box<dyn Fn(Question) + Send + Sync>),
}

impl std::fmt::Debug for Consent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Always => "Always",
            Self::Never => "Never",
            Self::Ask(_) => "Ask",
        })
    }
}

/// The answer to one request.
#[derive(Debug)]
pub struct Answer {
    /// The ssh-agent response to send back.
    pub message: Vec<u8>,
    /// Why a signature was refused without a no, for whoever is watching.
    pub refused: Option<String>,
    /// The key signed with unasked ([`Consent::Always`]).
    pub signed_unasked: Option<String>,
}

impl Answer {
    const fn new(message: Vec<u8>) -> Self {
        Self {
            message,
            refused: None,
            signed_unasked: None,
        }
    }

    fn failure() -> Self {
        Self::new(vec![SSH_AGENT_FAILURE])
    }

    fn refused(why: String) -> Self {
        Self {
            refused: Some(why),
            ..Self::failure()
        }
    }
}

/// What every request from one host is answered with.
#[derive(Debug)]
pub struct AgentServing {
    /// The local agent's socket.
    agent: PathBuf,
    /// Who is asking, for the prompt.
    host: String,
    /// The only fingerprints the host may use; `None` allows every key.
    keys: Option<Vec<String>>,
    consent: Consent,
    /// Serializes the questions.
    asking: Mutex<()>,
    in_flight: AtomicUsize,
}

impl AgentServing {
    /// Serve the agent at `agent` to `host`, limited to `keys` when set.
    #[must_use]
    pub const fn new(
        agent: PathBuf,
        host: String,
        keys: Option<Vec<String>>,
        consent: Consent,
    ) -> Self {
        Self {
            agent,
            host,
            keys,
            consent,
            asking: Mutex::new(()),
            in_flight: AtomicUsize::new(0),
        }
    }

    /// Answer `request` on a thread of its own, which the question may hold
    /// for as long as the server waits.
    pub fn handle(
        self: &Arc<Self>,
        request: AgentRequest,
        done: impl FnOnce(Answer) + Send + 'static,
    ) {
        let started = Instant::now();
        if self.in_flight.fetch_add(1, Ordering::AcqRel) >= MAX_IN_FLIGHT {
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
            return done(Answer::failure());
        }
        let serving = Arc::clone(self);
        std::thread::spawn(move || {
            let answer = serving.answer(&request, started);
            serving.in_flight.fetch_sub(1, Ordering::AcqRel);
            done(answer);
        });
    }

    /// The response to `request`, received at `started`. Anything refused,
    /// failed, or late is `SSH_AGENT_FAILURE`.
    fn answer(&self, request: &AgentRequest, started: Instant) -> Answer {
        let message = &request.message;
        match message.first() {
            Some(&SSH_AGENTC_REQUEST_IDENTITIES) => self
                .local(message, started)
                .and_then(|list| self.allowed(list))
                .map_or_else(Answer::failure, Answer::new),
            Some(&SSH_AGENTC_SIGN_REQUEST) => self.sign(request, started),
            // The server already refuses the rest; so does this.
            _ => Answer::failure(),
        }
    }

    /// `list` without the keys the host may not use.
    fn allowed(&self, list: Vec<u8>) -> Option<Vec<u8>> {
        let Some(keys) = &self.keys else {
            return Some(list);
        };
        let kept: Vec<_> = identities(&list)?
            .into_iter()
            .filter(|(blob, _)| keys.iter().any(|key| key.trim() == fingerprint(blob)))
            .collect();
        let mut out = vec![SSH_AGENT_IDENTITIES_ANSWER];
        out.extend(u32::try_from(kept.len()).ok()?.to_be_bytes());
        for part in kept.into_iter().flat_map(<[&[u8]; 2]>::from) {
            out.extend(u32::try_from(part.len()).ok()?.to_be_bytes());
            out.extend(part);
        }
        Some(out)
    }

    fn sign(&self, request: &AgentRequest, started: Instant) -> Answer {
        let Some(key) = sign_request_key(&request.message) else {
            return Answer::failure();
        };
        let print = fingerprint(key);
        let host = &self.host;
        let held = self.local(&[SSH_AGENTC_REQUEST_IDENTITIES], started);
        // Absent from the filtered listing: not held, or not allowed here.
        let Some(comment) = held
            .and_then(|list| self.allowed(list))
            .and_then(|list| comment_of(&list, key))
        else {
            return Answer::refused(format!(
                "{host}: refused a signature with {print}, a key not offered to it"
            ));
        };
        let key = format!("{print} ({})", printable(&comment));
        let mut unasked = None;
        match &self.consent {
            Consent::Always => unasked = Some(key),
            Consent::Never => {
                return Answer::refused(format!(
                    "{host}: refused a signature with {key}: there is no one to ask"
                ));
            }
            Consent::Ask(ask) => {
                let _one_at_a_time = self.asking.lock().unwrap_or_else(PoisonError::into_inner);
                let expires = started + DEADLINE;
                let wait = |by: Instant| by.saturating_duration_since(Instant::now());
                if wait(expires).is_zero() {
                    return Answer::refused(format!(
                        "{host}: a request to sign with {key} expired unanswered"
                    ));
                }
                let (answer, answered) = mpsc::sync_channel(1);
                ask(Question {
                    host: host.clone(),
                    pane: request.pane.clone(),
                    key,
                    expires,
                    answer,
                });
                if !answered.recv_timeout(wait(expires)).unwrap_or(false) {
                    return Answer::failure();
                }
                if wait(expires).is_zero() {
                    return Answer::refused(format!(
                        "{host}: too late, the server stopped waiting after \
                         {AGENT_REQUEST_SECS} s; nothing was signed"
                    ));
                }
            }
        }
        self.local(&request.message, started)
            .map_or_else(Answer::failure, |message| Answer {
                signed_unasked: unasked,
                ..Answer::new(message)
            })
    }

    /// `message`'s answer from the local agent, for as long as the server
    /// waits: an agent may ask its user first.
    fn local(&self, message: &[u8], started: Instant) -> Option<Vec<u8>> {
        local(&self.agent, message, started + DEADLINE).ok()
    }
}

/// The agent socket a client serves from: `named`, else `SSH_AUTH_SOCK`.
///
/// # Errors
///
/// Why there is none to serve, for the user.
pub fn local_agent(named: Option<&Path>) -> Result<PathBuf, String> {
    let socket = match named {
        Some(named) => named.to_path_buf(),
        None => std::env::var_os("SSH_AUTH_SOCK")
            .filter(|path| !path.is_empty())
            .ok_or("SSH_AUTH_SOCK is not set: there is no ssh-agent here to serve")?
            .into(),
    };
    match UnixStream::connect(&socket) {
        Ok(_) => Ok(socket),
        Err(err) => Err(format!(
            "no ssh-agent answers at {}: {err}",
            socket.display()
        )),
    }
}

/// One request and its answer from the local agent at `socket`.
fn local(socket: &Path, message: &[u8], by: Instant) -> std::io::Result<Vec<u8>> {
    let mut stream = UnixStream::connect(socket)?;
    // Zero would mean no timeout at all.
    let left = by.saturating_duration_since(Instant::now());
    let timeout = Some(left.max(Duration::from_millis(1)));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let len = u32::try_from(message.len()).map_err(std::io::Error::other)?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(message)?;
    let mut len = [0; 4];
    stream.read_exact(&mut len)?;
    let len = usize::try_from(u32::from_be_bytes(len)).map_err(std::io::Error::other)?;
    if len > MAX_AGENT_MESSAGE_BYTES {
        return Err(std::io::Error::other("the agent's answer is too long"));
    }
    let mut answer = vec![0; len];
    stream.read_exact(&mut answer)?;
    Ok(answer)
}

/// Every key in an identities answer with its comment.
fn identities(list: &[u8]) -> Option<Vec<(&[u8], &[u8])>> {
    fn string<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
        let (len, tail) = rest.split_first_chunk::<4>()?;
        let (value, tail) = tail.split_at_checked(u32::from_be_bytes(*len) as usize)?;
        *rest = tail;
        Some(value)
    }
    let rest = list.strip_prefix(&[SSH_AGENT_IDENTITIES_ANSWER])?;
    let (count, mut rest) = rest.split_first_chunk::<4>()?;
    let mut out = Vec::new();
    for _ in 0..u32::from_be_bytes(*count) {
        out.push((string(&mut rest)?, string(&mut rest)?));
    }
    Some(out)
}

/// The comment `list` holds `key` under; `None` when it does not hold it.
fn comment_of(list: &[u8], key: &[u8]) -> Option<String> {
    identities(list)?
        .into_iter()
        .find(|(blob, _)| *blob == key)
        .map(|(_, comment)| String::from_utf8_lossy(comment).into_owned())
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

/// The printable ASCII of text this program did not write, kept short.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests;
