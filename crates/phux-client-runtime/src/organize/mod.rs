//! The organize judge and its undo journal (ADR-0174), shared by every
//! client: the CLI and MCP through `phux_client::organize`, the TUI, and
//! the desktop.
//!
//! [`Judge::configured`] picks `TypeSafe`'s Jev (with `TYPESAFE_API_KEY`, or
//! the key `organize.jev-key-command` prints) or repository matching;
//! [`plan_or_fallback`] plans over [`Facts`] with it, falling back to
//! repository matching when Jev fails; [`save_journal`] / [`load_journal`]
//! keep the last run for undo. Everything here blocks: call it from a
//! blocking thread (`spawn_blocking`, a GPUI background task).
//!
//! The request goes through `curl` with the key and body on its stdin, so
//! neither appears in a process listing, and the binary carries no HTTP or
//! TLS stack for it.

pub mod sequencer;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use phux_client_core::organize::{
    self as planner, Candidates, Facts, Journal, JudgeKind, NewSession, PaneFacts, Plan, PlanInput,
    Scope, SessionFacts, Verdict,
};
use phux_core::project_identity::SessionProject;
use phux_protocol::ids::{ResourceKey, SessionKey};
use phux_protocol::wire::frame::SessionRecord;
use serde_json::Value;

/// The Jev evaluation endpoint.
pub const JEV_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// The environment variable holding the `TypeSafe` API key.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";

/// How long one Jev request may take by default.
pub const JEV_TIMEOUT: Duration = Duration::from_secs(60);

/// How long an interactive client waits on Jev before repository matching
/// decides.
pub const INTERACTIVE_JEV_TIMEOUT: Duration = Duration::from_secs(8);

/// How long `organize.jev-key-command` may take.
const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// How often a blocking wait polls its child.
const POLL: Duration = Duration::from_millis(20);

pub use phux_config::instance::local_host_name;
pub use sequencer::{Records, Report, SKIPPED, Sequencer, Step, Submitter, UndoRefusal};

/// One session record as the planner sees it; `intake` is the designated
/// Intake session.
#[must_use]
pub fn session_facts(record: &SessionRecord, intake: Option<SessionKey>) -> SessionFacts {
    let project = SessionProject::from_attrs(
        record
            .attrs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    SessionFacts {
        key: record.key,
        name: record.name.clone(),
        intake: intake == Some(record.key),
        project_id: project.as_ref().map(|p| p.id.clone()),
        project_name: project.as_ref().map(|p| p.name.clone()),
        checkout: project.map(|p| p.checkout),
    }
}

/// What a client knows about one terminal, before repository discovery.
#[derive(Debug, Clone, Default)]
pub struct PaneSeen {
    /// How a person addresses it (`@3`, else `^ab23cd`).
    pub label: String,
    /// Its name or title.
    pub title: Option<String>,
    /// Its working directory.
    pub cwd: Option<String>,
    /// The agent running there and its state.
    pub agent: Option<String>,
    /// Its state in words.
    pub status: String,
}

/// One terminal as the planner sees it, its repository read through
/// `projects`.
#[must_use]
pub fn pane_facts(
    key: ResourceKey,
    session: SessionKey,
    seen: PaneSeen,
    projects: &mut ProjectCache,
) -> PaneFacts {
    let project = seen.cwd.as_deref().and_then(|cwd| projects.of(cwd));
    PaneFacts {
        key,
        label: seen.label,
        session,
        title: seen.title,
        cwd: seen.cwd,
        agent: seen.agent,
        status: seen.status,
        project_id: project.as_ref().map(|p| p.id.clone()),
        project_name: project.as_ref().map(|p| p.name.clone()),
        checkout: project.map(|p| p.checkout),
    }
}

/// Repository discovery per working directory, read once each, and only
/// when the server shares this machine's filesystem.
#[derive(Debug, Default)]
pub struct ProjectCache {
    host: Option<String>,
    seen: HashMap<String, Option<SessionProject>>,
}

impl ProjectCache {
    /// Discovery on `host` (this machine's name), or none for `None` (a
    /// remote server, whose paths are not ours to read).
    #[must_use]
    pub fn new(host: Option<&str>) -> Self {
        Self {
            host: host.map(str::to_owned),
            seen: HashMap::new(),
        }
    }

    /// The repository `cwd` is in.
    pub fn of(&mut self, cwd: &str) -> Option<SessionProject> {
        let host = self.host.as_deref()?;
        self.seen
            .entry(cwd.to_owned())
            .or_insert_with(|| crate::project_identity::discover(Path::new(cwd), host, None))
            .clone()
    }
}

/// Which judge to ask.
#[derive(Debug, Clone)]
pub enum Judge {
    /// `TypeSafe`'s Jev, with this API key.
    Jev(String),
    /// Repository and directory matching only.
    Heuristic,
}

impl Judge {
    /// Jev when [`API_KEY_ENV`] is set and `offline` is false, else the
    /// heuristic.
    #[must_use]
    pub fn from_env(offline: bool) -> Self {
        if offline {
            return Self::Heuristic;
        }
        std::env::var(API_KEY_ENV)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .map_or(Self::Heuristic, Self::Jev)
    }

    /// The judge `config` and `offline` allow.
    ///
    /// The heuristic when either is offline; Jev with [`API_KEY_ENV`], else
    /// with the key `organize.jev-key-command` prints; else the heuristic.
    /// The second value says why Jev was wanted but is not used.
    #[must_use]
    pub fn configured(config: &phux_config::OrganizeCfg, offline: bool) -> (Self, Option<String>) {
        let offline = offline || config.offline;
        let judge = Self::from_env(offline);
        let argv = config.jev_key_command.as_deref().unwrap_or_default();
        if matches!(judge, Self::Jev(_)) || offline || argv.is_empty() {
            return (judge, None);
        }
        match key_from_command(argv) {
            Ok(key) => (Self::Jev(key), None),
            Err(why) => (
                Self::Heuristic,
                Some(format!("organize.jev-key-command: {why}")),
            ),
        }
    }

    /// [`Self::configured`] with the `[organize]` table of the user's
    /// config file (defaults when it does not load).
    #[must_use]
    pub fn from_config_file(offline: bool) -> (Self, Option<String>) {
        let config = phux_config::loader::load().unwrap_or_default();
        Self::configured(&config.organize, offline)
    }

    /// Which kind of judge this is.
    #[must_use]
    pub const fn kind(&self) -> JudgeKind {
        match self {
            Self::Jev(_) => JudgeKind::Jev,
            Self::Heuristic => JudgeKind::Heuristic,
        }
    }
}

/// How to plan.
#[derive(Debug, Clone)]
pub struct Options {
    /// Which terminals may move: every one, or one session's.
    pub session: Option<String>,
    /// Who decides.
    pub judge: Judge,
    /// Confidence a verdict needs to move a terminal.
    pub min_confidence: f64,
    /// How long the judge may take in all, across every Jev request, before
    /// the heuristic decides.
    pub jev_timeout: Duration,
}

/// Why planning failed; nothing has changed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// The session named as the scope does not exist.
    #[error("no such session: {0}")]
    NoSuchSession(String),
    /// Jev could not be reached or answered with an error.
    #[error("jev: {0}")]
    Jev(String),
}

/// Plan an organize run over `facts` with `options.judge`.
///
/// # Errors
///
/// [`PlanError::NoSuchSession`] for an unknown scope; [`PlanError::Jev`]
/// when Jev fails.
pub fn plan_with(facts: &Facts, options: &Options) -> Result<Plan, PlanError> {
    let scope = facts
        .scope(options.session.as_deref())
        .ok_or_else(|| PlanError::NoSuchSession(options.session.clone().unwrap_or_default()))?;
    let candidates = planner::candidates(&facts.sessions, &facts.panes, scope);
    let verdicts = match &options.judge {
        Judge::Jev(key) => jev_verdicts(key, facts, scope, &candidates, options.jev_timeout)?,
        Judge::Heuristic => planner::heuristic_verdicts(&facts.panes, scope, &candidates),
    };
    Ok(planner::plan(
        PlanInput {
            panes: &facts.panes,
            scope,
            candidates: &candidates,
            judge: options.judge.kind(),
            min_confidence: options.min_confidence,
        },
        &verdicts,
    ))
}

/// [`plan_with`], falling back to the heuristic when Jev fails: the plan,
/// and why Jev was not used when it was asked for.
///
/// # Errors
///
/// [`PlanError::NoSuchSession`] for an unknown scope.
pub fn plan_or_fallback(
    facts: &Facts,
    options: &Options,
) -> Result<(Plan, Option<String>), PlanError> {
    match plan_with(facts, options) {
        Err(PlanError::Jev(why)) => {
            let fallback = Options {
                judge: Judge::Heuristic,
                ..options.clone()
            };
            Ok((plan_with(facts, &fallback)?, Some(why)))
        }
        other => other.map(|plan| (plan, None)),
    }
}

fn jev_verdicts(
    key: &str,
    facts: &Facts,
    scope: Scope,
    candidates: &Candidates,
    timeout: Duration,
) -> Result<Vec<Verdict>, PlanError> {
    let deadline = Instant::now() + timeout;
    let mut responses = Vec::new();
    for body in planner::jev_request(&facts.panes, scope, candidates) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left < Duration::from_secs(1) {
            return Err(PlanError::Jev("timed out".to_owned()));
        }
        responses.push(jev_post(key, &body, left)?);
    }
    planner::read_jev_answers(&facts.panes, scope, candidates, &responses)
        .map_err(|err| PlanError::Jev(err.to_string()))
}

/// The attributes a created session carries: the repository identity it
/// was proposed for (ADR-0168), each value within the wire bound.
#[must_use]
pub fn creation_attrs(new: &NewSession) -> Vec<(String, String)> {
    phux_core::project_identity::SessionProject {
        id: new.project_id.clone(),
        name: new.project_name.clone(),
        checkout: new.checkout.clone(),
        branch: None,
        linked_worktree: false,
        host: None,
    }
    .to_attrs()
}

/// The first line of `argv`'s stdout, which must exit 0 and print a key: no
/// whitespace or control characters (a password manager's later lines are
/// metadata, never part of it).
fn key_from_command(argv: &[String]) -> Result<String, String> {
    let (program, rest) = argv.split_first().ok_or("empty command")?;
    let child = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("cannot run {program}: {err}"))?;
    let (ok, stdout, _) = finish(child, KEY_COMMAND_TIMEOUT)?;
    if !ok {
        return Err(format!("{program} failed"));
    }
    let text = String::from_utf8_lossy(&stdout);
    let key = text.lines().next().unwrap_or_default().trim();
    if key.is_empty() {
        return Err(format!("{program} printed nothing"));
    }
    if key.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err(format!("{program} printed something that is not a key"));
    }
    Ok(key.to_owned())
}

/// POST one request body to Jev through `curl`.
fn jev_post(key: &str, body: &Value, timeout: Duration) -> Result<Value, PlanError> {
    // `-q` first: the user's `.curlrc` (a `verbose`, an `include`) never
    // applies, so the key cannot reach stderr and the reply stays parseable.
    let mut child = Command::new("curl")
        .args(["-q", "--silent", "--show-error", "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| PlanError::Jev(format!("cannot run curl: {err}")))?;
    let config = curl_config(key, body, timeout);
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|err| PlanError::Jev(format!("cannot write to curl: {err}")))?;
    }
    let (ok, stdout, stderr) =
        finish(child, timeout + Duration::from_secs(2)).map_err(PlanError::Jev)?;
    read_jev_reply(&stdout, &stderr, ok).map_err(|err| redact(err, key))
}

/// `err` with every occurrence of `key` replaced.
fn redact(err: PlanError, key: &str) -> PlanError {
    let key = key.trim();
    match err {
        PlanError::Jev(why) if !key.is_empty() => PlanError::Jev(why.replace(key, "***")),
        other => other,
    }
}

/// Wait for `child` up to `timeout`, reading its output on helper threads
/// so a full pipe cannot stall it; kill it at the deadline.
fn finish(mut child: Child, timeout: Duration) -> Result<(bool, Vec<u8>, Vec<u8>), String> {
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(|err| err.to_string())? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".to_owned());
            }
            None => std::thread::sleep(POLL),
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok((status.success(), stdout, stderr))
}

/// A curl config that POSTs `body` to [`JEV_ENDPOINT`] with `key`. Values
/// are quoted with curl's config escapes, so no newline or quote in a pane
/// title can start another option.
fn curl_config(key: &str, body: &Value, timeout: Duration) -> String {
    format!(
        "url = \"{JEV_ENDPOINT}\"\nrequest = \"POST\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"{}\"\nwrite-out = \"\\n%{{http_code}}\"\nmax-time = {}\n",
        curl_quote(key.trim()),
        curl_quote(&body.to_string()),
        timeout.as_secs().max(1),
    )
}

/// `text` inside a double-quoted curl config value.
fn curl_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// The JSON body of a curl run whose stdout ends with `\n<http status>`.
/// An error body is not echoed: it could carry the request back.
fn read_jev_reply(stdout: &[u8], stderr: &[u8], ran: bool) -> Result<Value, PlanError> {
    if !ran {
        let detail = String::from_utf8_lossy(stderr);
        let first = detail.lines().next().unwrap_or_default().trim();
        return Err(PlanError::Jev(format!("request failed: {first}")));
    }
    let text = String::from_utf8_lossy(stdout);
    let text = text.trim_end();
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", text));
    let status: u16 = status.trim().parse().unwrap_or(0);
    if status != 200 {
        return Err(PlanError::Jev(format!("HTTP {status}")));
    }
    serde_json::from_str(body).map_err(|err| PlanError::Jev(format!("unreadable reply: {err}")))
}

/// Where the last organize run's [`Journal`] is kept for this profile.
#[must_use]
pub fn journal_path() -> PathBuf {
    phux_config::instance::state_dir()
        .join("organize")
        .join("last.json")
}

/// Keep `journal` as the one undo reverses next.
///
/// # Errors
///
/// The file could not be written.
pub fn save_journal(journal: &Journal) -> std::io::Result<()> {
    save_journal_at(&journal_path(), journal)
}

fn save_journal_at(path: &Path, journal: &Journal) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec_pretty(journal).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

/// The kept [`Journal`], if there is one.
///
/// # Errors
///
/// The file exists but could not be read or parsed.
pub fn load_journal() -> std::io::Result<Option<Journal>> {
    load_journal_at(&journal_path())
}

fn load_journal_at(path: &Path) -> std::io::Result<Option<Journal>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(std::io::Error::other),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Forget the kept [`Journal`] after it was undone.
///
/// # Errors
///
/// The file exists but could not be removed.
pub fn clear_journal() -> std::io::Result<()> {
    clear_journal_at(&journal_path())
}

fn clear_journal_at(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_client_core::organize::CreatedSession;
    use phux_protocol::ids::SessionKey;

    use super::*;

    #[test]
    fn curl_config_escapes_values_so_nothing_starts_a_new_option() {
        let config = curl_config(
            "k\"ey\n",
            &serde_json::json!({"a": "b\\c\"d\nurl = \"https://evil\""}),
            JEV_TIMEOUT,
        );
        assert!(
            config.contains(r#"header = "Authorization: Bearer k\"ey""#),
            "{config}"
        );
        assert_eq!(config.lines().filter(|l| l.starts_with("url =")).count(), 1);
        assert_eq!(config.lines().count(), 7, "{config}");
    }

    #[test]
    fn jev_replies_carry_their_status_and_never_echo_the_body() {
        let ok = read_jev_reply(b"{\"answers\":{}}\n200", b"", true).expect("ok");
        assert!(ok.get("answers").is_some());
        let refused = read_jev_reply(b"{\"error\":\"secret\"}\n401", b"", true).unwrap_err();
        assert_eq!(refused, PlanError::Jev("HTTP 401".into()));
        let failed = read_jev_reply(b"", b"curl: (6) no host\nmore", false).unwrap_err();
        assert_eq!(
            failed,
            PlanError::Jev("request failed: curl: (6) no host".into())
        );
    }

    #[test]
    fn a_key_command_must_succeed_and_print_something() {
        let argv = |line: &str| vec!["sh".to_owned(), "-c".to_owned(), line.to_owned()];
        assert_eq!(
            key_from_command(&argv("printf ' k1 \\n'")).as_deref(),
            Ok("k1")
        );
        assert!(key_from_command(&argv("true")).is_err());
        assert_eq!(
            key_from_command(&argv("printf 'k2\\nurl: https://evil\\n'")).as_deref(),
            Ok("k2")
        );
        assert!(key_from_command(&argv("echo 'a b'")).is_err());
        assert!(key_from_command(&argv("echo k; exit 3")).is_err());
        assert!(key_from_command(&[]).is_err());
    }

    #[test]
    fn errors_never_carry_the_key() {
        let err = redact(PlanError::Jev("Bearer sk-123 rejected".into()), "sk-123\n");
        assert_eq!(err, PlanError::Jev("Bearer *** rejected".into()));
    }

    #[test]
    fn created_sessions_carry_a_whole_identity() {
        let attrs = creation_attrs(&NewSession {
            name: "phux-2".into(),
            project_id: "github.com/no-phux/phux".into(),
            project_name: "phux".into(),
            checkout: "/src/phux".into(),
        });
        let project = phux_core::project_identity::SessionProject::from_attrs(
            attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )
        .expect("identity");
        assert_eq!(project.name, "phux");
    }

    #[test]
    fn offline_never_runs_the_key_command() {
        let config = phux_config::OrganizeCfg {
            jev_key_command: Some(vec!["false".to_owned()]),
            offline: true,
        };
        let (judge, why) = Judge::configured(&config, false);
        assert!(matches!(judge, Judge::Heuristic));
        assert_eq!(why, None);
    }

    #[test]
    fn an_unknown_scope_is_refused_before_judging() {
        let options = Options {
            session: Some("nope".into()),
            judge: Judge::Heuristic,
            min_confidence: 0.6,
            jev_timeout: JEV_TIMEOUT,
        };
        assert_eq!(
            plan_or_fallback(&Facts::default(), &options).unwrap_err(),
            PlanError::NoSuchSession("nope".into())
        );
    }

    /// A journal survives a write and read, and clearing it twice is fine.
    #[test]
    fn journal_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("organize").join("last.json");
        assert_eq!(load_journal_at(&path).expect("read"), None);
        let mut journal = Journal::new();
        journal.created.push(CreatedSession {
            key: SessionKey::new([1; 16]).expect("non-zero"),
            name: "phux".into(),
        });
        save_journal_at(&path, &journal).expect("save");
        assert_eq!(load_journal_at(&path).expect("read"), Some(journal));
        clear_journal_at(&path).expect("clear");
        clear_journal_at(&path).expect("clear twice");
    }
}
