//! `phux agent stats`: how much work each pane's agent has done (tasks
//! finished, time spent working), and with `--publish` the
//! `phux.agent-stats/v1` Global metadata key the agent-stats plugin keeps
//! current from its hooks. Every hook run is a fresh process, so the running
//! tallies live in a state file under the profile's state directory, one per
//! server socket, rewritten under an exclusive lock.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use phux_client::attach::AttachError;
use phux_client::global_metadata::{self, WriteError};
use phux_server::runtime::default_socket_path;
use serde::{Deserialize, Serialize};

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::server_target::ServerTarget;

/// The Global key the document is published under.
pub(crate) const STATS_KEY: &str = "phux.agent-stats/v1";
/// Keep at most this many agents, the most recently updated.
const MAX_AGENTS: usize = 128;
/// The largest document written; the server floor for a value is 4 KiB but
/// its default is 256 KiB, and every write reaches every watching client.
const MAX_DOCUMENT_BYTES: usize = 16 * 1024;
/// Longer agent names are cut to this many characters.
const MAX_NAME_CHARS: usize = 48;

/// The tallies kept between hook runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    agents: BTreeMap<String, Agent>,
}

/// One pane's agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Agent {
    kind: String,
    name: String,
    turns: u64,
    /// Closed `working` intervals, in ms.
    working_ms: i64,
    /// The last state seen, and when it began.
    state: String,
    since_ms: i64,
    first_seen_ms: i64,
    updated_ms: i64,
}

impl Agent {
    fn working_s(&self, now_ms: i64) -> i64 {
        let open = if self.state == "working" {
            (now_ms - self.since_ms).max(0)
        } else {
            0
        };
        (self.working_ms + open) / 1000
    }
}

/// The hook event this run records, from its environment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    State {
        terminal: String,
        kind: String,
        name: String,
        from: Option<String>,
        to: String,
    },
    Exit {
        terminal: String,
    },
    /// Anything else (`client-attached`, a manual run): republish only.
    None,
}

impl Event {
    fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        let Some(terminal) = var("PHUX_TERMINAL_ID").filter(|id| !id.is_empty()) else {
            return Self::None;
        };
        match var("PHUX_EVENT").as_deref() {
            Some("agent-state-changed") => Self::State {
                terminal,
                kind: var("PHUX_AGENT_KIND").unwrap_or_default(),
                name: var("PHUX_AGENT_NAME").unwrap_or_default(),
                from: var("PHUX_FROM").filter(|from| !from.is_empty()),
                to: var("PHUX_TO").unwrap_or_else(|| "unknown".to_owned()),
            },
            Some("pane-exit") => Self::Exit { terminal },
            _ => Self::None,
        }
    }
}

impl State {
    /// Record `event`, seen at `now_ms`.
    ///
    /// A finished task is any move into `done`, or `working` to `idle` for
    /// agents that never say done; `done` to `idle` is the same task, so it
    /// does not count again. Time in `working` is added when the agent
    /// leaves it.
    fn apply(&mut self, event: Event, now_ms: i64) {
        let (terminal, kind, name, from, to) = match event {
            Event::State {
                terminal,
                kind,
                name,
                from,
                to,
            } => (terminal, kind, name, from, to),
            Event::Exit { terminal } => {
                self.agents.remove(&terminal);
                return;
            }
            Event::None => return,
        };
        if to == "unknown" && !self.agents.contains_key(&terminal) {
            return;
        }
        let agent = self.agents.entry(terminal).or_default();
        if !kind.is_empty() && !agent.kind.is_empty() && agent.kind != kind {
            *agent = Agent::default();
        }
        if agent.first_seen_ms == 0 {
            agent.first_seen_ms = now_ms;
        }
        if to == "done" || (from.as_deref() == Some("working") && to == "idle") {
            agent.turns += 1;
        }
        if !kind.is_empty() {
            agent.kind = kind;
        }
        if !name.is_empty() {
            agent.name = name.chars().take(MAX_NAME_CHARS).collect();
        }
        // ponytail: hooks carry no event time, so a run's start time orders
        // them; a run that lost the lock race to a later event counts its
        // task but leaves the state and timer alone.
        if now_ms < agent.updated_ms {
            return;
        }
        if agent.state == "working" && to != "working" {
            agent.working_ms += (now_ms - agent.since_ms).max(0);
        }
        if agent.state != to {
            agent.state = to;
            agent.since_ms = now_ms;
        }
        agent.updated_ms = now_ms;
    }

    /// Drop the least recently updated agents past [`MAX_AGENTS`].
    fn cap(&mut self) {
        while self.agents.len() > MAX_AGENTS {
            self.drop_oldest();
        }
    }

    fn drop_oldest(&mut self) {
        let oldest = self
            .agents
            .iter()
            .min_by_key(|(_, agent)| agent.updated_ms)
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest {
            self.agents.remove(&id);
        }
    }

    /// The published document at `now_ms`, compact JSON.
    fn document(&self, now_ms: i64) -> Vec<u8> {
        let agents: serde_json::Map<String, serde_json::Value> = self
            .agents
            .iter()
            .map(|(id, agent)| {
                let value = serde_json::json!({
                    "kind": agent.kind,
                    "name": agent.name,
                    "turns": agent.turns,
                    "working_s": agent.working_s(now_ms),
                    "first_seen_ms": agent.first_seen_ms,
                    "updated_ms": agent.updated_ms,
                });
                (id.clone(), value)
            })
            .collect();
        serde_json::to_vec(&serde_json::json!({ "v": 1, "agents": agents })).unwrap_or_default()
    }

    /// [`Self::document`], dropping the oldest agents until it fits.
    fn bounded_document(&mut self, now_ms: i64) -> Vec<u8> {
        self.cap();
        loop {
            let bytes = self.document(now_ms);
            if bytes.len() <= MAX_DOCUMENT_BYTES || self.agents.is_empty() {
                return bytes;
            }
            self.drop_oldest();
        }
    }
}

/// The state file for one server, held under an exclusive lock for the run.
#[derive(Debug)]
struct Store {
    path: PathBuf,
    _lock: File,
}

impl Store {
    /// Lock `dir` and read the state for `socket`. Waits for another run:
    /// each holds the lock for milliseconds, and the server kills a stuck
    /// one after 30 seconds, which releases it.
    fn open(dir: &Path, socket: &Path) -> std::io::Result<(Self, State)> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        lock.lock()?;
        let name: String = socket
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = dir.join(format!("{name}.json"));
        let state = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok((Self { path, _lock: lock }, state))
    }

    /// Replace the state file atomically.
    fn save(&self, state: &State) -> std::io::Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(state).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// `phux agent stats [--publish]`.
///
/// Prints the document for this server. `--publish` first records the hook
/// event in the environment, then writes the key, all under the state lock
/// so an older document never overwrites a newer one. A server without the
/// key is a fresh start (terminal ids restart with the server), so the
/// tallies reset.
pub(crate) fn run_stats(publish: bool, socket: Option<PathBuf>) -> ExitCode {
    let now_ms = now_ms();
    let event = Event::from_env(|name| std::env::var(name).ok());
    let socket = socket.unwrap_or_else(default_socket_path);
    let dir = phux_config::instance::state_dir().join("agent-stats");
    let (store, mut state) = match Store::open(&dir, &socket) {
        Ok(opened) => opened,
        Err(err) => {
            let err = CliError::new(
                codes::TRANSPORT,
                format!("cannot open agent stats at {}: {err}", dir.display()),
                "check the directory is writable",
            );
            return json_err::emit(false, &err, 1);
        }
    };
    if !publish {
        outln!(
            "{}",
            String::from_utf8_lossy(&state.bounded_document(now_ms))
        );
        return ExitCode::SUCCESS;
    }
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let target = ServerTarget::local(&socket);
    match rt.block_on(read_published(&target)) {
        Ok(true) => {}
        Ok(false) => state = State::default(),
        Err(err) => return target.report_unreachable(false, &err, "agent stats"),
    }
    state.apply(event, now_ms);
    let bytes = state.bounded_document(now_ms);
    if let Err(err) = store.save(&state) {
        warn!(
            "agent stats: could not save {}: {err}",
            store.path.display()
        );
    }
    match rt.block_on(write(&target, bytes)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(WriteError::Refused(refusal)) => {
            let err = CliError::new(
                codes::TRANSPORT,
                format!("the server refused {STATS_KEY}: {refusal}"),
                "check this connection's grant covers `bind@global`",
            );
            json_err::emit(false, &err, 1)
        }
        Err(WriteError::Attach(err)) => target.report_unreachable(false, &err, "agent stats"),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Whether the server already holds the key.
async fn read_published(target: &ServerTarget) -> Result<bool, AttachError> {
    let mut conn = target.connect().await?;
    let values = global_metadata::read(&mut conn, &[STATS_KEY]).await?;
    Ok(values.into_iter().next().flatten().is_some())
}

async fn write(target: &ServerTarget, value: Vec<u8>) -> Result<(), WriteError> {
    let mut conn = target.connect().await?;
    global_metadata::write(&mut conn, STATS_KEY, value).await
}

#[cfg(test)]
mod tests;
