//! The `min` CLI: list sessions, probe a box, and redeem a join code in one.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// What `phux host join` runs inside a box: start its server if the
/// loadout's activation hook has not, then redeem the code read from stdin.
const JOIN_SCRIPT: &str = "phux server --ensure >/dev/null 2>&1; exec phux host join -";

/// The longest a `min` call may take before it is killed, so a wedged box
/// or daemon can never stall `watch`. A join dials the hub and enrolls.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const JOIN_TIMEOUT: Duration = Duration::from_secs(60);

/// One Minimal session, from `min ls --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BoxSession {
    pub(super) id: String,
    pub(super) name: Option<String>,
    status: String,
}

impl BoxSession {
    #[cfg(test)]
    pub(super) fn new(id: &str, name: Option<&str>, status: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.map(str::to_owned),
            status: status.to_owned(),
        }
    }

    /// Only an active box can run a command.
    pub(super) fn is_active(&self) -> bool {
        self.status == "active"
    }

    /// The session's name, else its id, for messages.
    pub(super) fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// A resolved `min` executable.
#[derive(Debug, Clone)]
pub(super) struct MinCli {
    program: PathBuf,
}

impl MinCli {
    /// `explicit`, else `min` on `PATH`, else the installer's
    /// `~/.local/bin/min` (a service's `PATH` rarely has it).
    pub(super) fn resolve(explicit: Option<&Path>) -> Result<Self, String> {
        let candidate = explicit.map(Path::to_path_buf).or_else(|| {
            let on_path = std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("min"))
                    .find(|path| path.is_file())
            });
            on_path.or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local/bin/min"))
                    .filter(|path| path.is_file())
            })
        });
        match candidate {
            Some(program) if program.is_file() => Ok(Self {
                program: std::path::absolute(&program).unwrap_or(program),
            }),
            Some(program) => Err(format!("{} is not the Minimal CLI", program.display())),
            None => Err("the Minimal CLI `min` is not installed".to_owned()),
        }
    }

    pub(super) fn program(&self) -> &Path {
        &self.program
    }

    /// Every session the Minimal daemon reports.
    pub(super) fn sessions(&self) -> Result<Vec<BoxSession>, String> {
        let child = self
            .command()
            .args(["--no-input", "session", "list", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("could not run {}: {err}", self.program.display()))?;
        let output = finish(child, LIST_TIMEOUT, "`min session list`")?;
        if !output.status.success() {
            return Err(format!(
                "`min session list` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        parse_sessions(&String::from_utf8_lossy(&output.stdout))
    }

    /// Whether the box has `phux` on its `PATH`.
    pub(super) fn has_phux(&self, session: &str) -> bool {
        self.exec(session, "command -v phux >/dev/null")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
            .and_then(|child| finish(child, PROBE_TIMEOUT, "the probe").ok())
            .is_some_and(|output| output.status.success())
    }

    /// Run `phux host join -` in the box with `code` on its stdin.
    pub(super) fn join(&self, session: &str, code: &str) -> Result<(), String> {
        let mut child = self
            .exec(session, JOIN_SCRIPT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("could not run {}: {err}", self.program.display()))?;
        if let Some(mut stdin) = child.stdin.take() {
            writeln!(stdin, "{code}")
                .map_err(|err| format!("could not hand over the code: {err}"))?;
        }
        let output = finish(child, JOIN_TIMEOUT, "`phux host join` in the box")?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!("{} {}", stderr.trim(), stdout.trim())
            .trim()
            .to_owned())
    }

    /// `min` with its own directory first on `PATH`: it starts its VM
    /// daemon (`minvmd`) from there, and a service's `PATH` rarely has it.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        if let Some(dir) = self.program.parent() {
            let rest = std::env::var_os("PATH").unwrap_or_default();
            let path = std::env::join_paths(
                std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&rest)),
            );
            if let Ok(path) = path {
                command.env("PATH", path);
            }
        }
        command
    }

    /// `min session exec SESSION sh -c SCRIPT`, the argv form, so no shell on
    /// the host or in the box re-splits the script.
    fn exec(&self, session: &str, script: &str) -> Command {
        let mut command = self.command();
        command.args(["--no-input", "session", "exec", session, "sh", "-c", script]);
        command
    }
}

/// Wait for `child` and collect its output, killing it past `limit`. The
/// pipes are drained on their own threads so a chatty child cannot block.
fn finish(mut child: Child, limit: Duration, what: &str) -> Result<Output, String> {
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{what} took longer than {}s and was stopped",
                    limit.as_secs()
                ));
            }
            Err(err) => return Err(format!("could not wait for {what}: {err}")),
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Parse `min session list --json`: `{"sessions":[{"id","name","status"}]}`.
/// Unknown fields are ignored; a session without an id is skipped.
pub(super) fn parse_sessions(body: &str) -> Result<Vec<BoxSession>, String> {
    let doc: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| format!("`min session list --json` printed something else: {err}"))?;
    let sessions = doc
        .get("sessions")
        .and_then(serde_json::Value::as_array)
        .ok_or("`min session list --json` has no `sessions` array")?;
    Ok(sessions
        .iter()
        .filter_map(|session| {
            let text = |key: &str| {
                session
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            Some(BoxSession {
                id: text("id").filter(|id| !id.is_empty())?,
                name: text("name").filter(|name| !name.is_empty()),
                status: text("status").unwrap_or_default(),
            })
        })
        .collect())
}
