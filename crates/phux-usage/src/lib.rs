//! Plan-usage collector behind `phux usage`.
//!
//! Reads each AI subscription's limit windows from the vendor's own local
//! credentials and builds one [`Snapshot`], the `phux.usage/v1` document a
//! host plugin publishes as a Global L3 metadata key. Nothing here speaks
//! the phux wire or runs inside the server.
//!
//! Sources: Claude Code's OAuth usage endpoint, Codex's rollout ledgers
//! (with the `ChatGPT` usage endpoint when they are old), and Grok Build's
//! billing endpoint. Credentials are read, never refreshed or written, and
//! sent only to the vendor that issued them. The Codex and Claude parsing
//! rules follow token-tach (MIT, Copyright 2026 phall).

#![expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) in private modules keeps unreachable_pub quiet"
)]

mod claude;
mod codex;
mod grok;
pub mod http;
mod model;
pub mod state;
mod util;

use std::path::PathBuf;

pub use model::{
    MAX_DOCUMENT_BYTES, Plan, REFRESH_KEY, SCHEMA, Snapshot, Status, USAGE_KEY, Window,
};
pub use state::{State, Store};

use http::Fetch;
use state::{STALE_AFTER_MS, SourceState};

/// Where each vendor keeps its credentials and ledgers.
#[derive(Debug, Clone)]
pub struct Sources {
    /// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub claude_dir: PathBuf,
    /// `~/.claude.json` (the account record).
    pub claude_account: PathBuf,
    /// Read the macOS Keychain item Claude Code stores its token in.
    pub keychain: bool,
    /// `$CODEX_HOME`, else `~/.codex`.
    pub codex_home: PathBuf,
    /// `~/.grok`.
    pub grok_home: PathBuf,
    /// Grok Build's CLI proxy (`$GROK_CLI_CHAT_PROXY_BASE_URL`).
    pub grok_proxy: String,
}

impl Sources {
    /// The locations the vendors' own clients use for this user.
    #[must_use]
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        let claude_dir =
            var("CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from);
        let claude_account = var("CLAUDE_CONFIG_DIR").map_or_else(
            || home.join(".claude.json"),
            |dir| PathBuf::from(dir).join(".claude.json"),
        );
        Self {
            claude_dir,
            claude_account,
            keychain: cfg!(target_os = "macos"),
            codex_home: var("CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from),
            grok_home: home.join(".grok"),
            grok_proxy: std::env::var("GROK_CLI_CHAT_PROXY_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| grok::DEFAULT_PROXY.to_owned()),
        }
    }
}

/// One collection pass.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    /// This host's short name.
    pub host: &'a str,
    /// Now (Unix ms).
    pub now_ms: i64,
    /// Skip each source's minimum interval (never a 429 backoff).
    pub eager: bool,
    /// The HTTP client.
    pub fetch: &'a dyn Fetch,
}

impl std::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("host", &self.host)
            .field("now_ms", &self.now_ms)
            .field("eager", &self.eager)
            .finish_non_exhaustive()
    }
}

/// Read every source this host has and build the snapshot. Network sources
/// run in parallel; `state` carries their intervals and backoff.
#[must_use]
pub fn collect(sources: &Sources, state: &mut State, request: Request<'_>) -> Snapshot {
    let mut claude_state = state.sources.remove(claude::SOURCE).unwrap_or_default();
    let mut grok_state = state.sources.remove(grok::SOURCE).unwrap_or_default();
    let mut wham_state = state.sources.remove(codex::WHAM_SOURCE).unwrap_or_default();
    let plans = std::thread::scope(|scope| {
        let claude = scope.spawn(|| claude::collect(sources, &mut claude_state, request));
        let grok = scope.spawn(|| grok::collect(sources, &mut grok_state, request));
        let codex = codex::collect(sources, &mut wham_state, request);
        [
            claude.join().ok().flatten(),
            codex,
            grok.join().ok().flatten(),
        ]
    });
    state
        .sources
        .insert(claude::SOURCE.to_owned(), claude_state);
    state.sources.insert(grok::SOURCE.to_owned(), grok_state);
    state
        .sources
        .insert(codex::WHAM_SOURCE.to_owned(), wham_state);
    Snapshot {
        schema: SCHEMA.to_owned(),
        host: request.host.to_owned(),
        observed_at_ms: request.now_ms,
        plans: plans.into_iter().flatten().collect(),
    }
}

/// Why one vendor request produced no windows.
#[derive(Debug)]
enum Failure {
    /// HTTP 429.
    Throttled,
    /// HTTP 401/403: the stored credential is no longer accepted.
    Rejected(String),
    /// Anything else.
    Failed(String),
}

impl Failure {
    fn from_status(status: u16, vendor: &str) -> Self {
        match status {
            429 => Self::Throttled,
            401 | 403 => Self::Rejected(format!("{vendor} rejected the stored sign-in")),
            other => Self::Failed(format!("{vendor} answered HTTP {other}")),
        }
    }
}

/// Run one rate-limited vendor request under `source`'s interval and
/// backoff, falling back to its last good windows when the request is not
/// due or fails. `identity` carries everything but the windows.
fn polled(
    source: &mut SourceState,
    identity: Plan,
    request: Request<'_>,
    read: impl FnOnce() -> Result<Vec<Window>, Failure>,
) -> Plan {
    let now = request.now_ms;
    if !source.due(now, request.eager) {
        return cached(source, identity, now);
    }
    source.attempted(now);
    match read() {
        Ok(windows) => {
            let plan = Plan {
                status: Status::Ok,
                error: None,
                fetched_at_ms: Some(now),
                windows,
                ..identity
            };
            source.succeeded(plan.clone());
            plan
        }
        Err(Failure::Throttled) => {
            source.throttled(now);
            source.last_error = None;
            cached(source, identity, now)
        }
        Err(Failure::Rejected(why)) => {
            source.failed(why, true);
            cached(source, identity, now)
        }
        Err(Failure::Failed(why)) => {
            source.failed(why, false);
            cached(source, identity, now)
        }
    }
}

/// The last good windows with a status saying how much to trust them.
fn cached(source: &SourceState, identity: Plan, now: i64) -> Plan {
    if now < source.retry_at_ms {
        let why = format!(
            "rate limited, retry in {}",
            util::human_wait(source.retry_at_ms - now)
        );
        let status = if source.last_good.is_some() {
            Status::Stale
        } else {
            Status::Error
        };
        return with_last_good(source, identity, status, Some(why));
    }
    if let Some(why) = source.last_error.clone() {
        let status = if source.rejected {
            Status::Unavailable
        } else {
            Status::Error
        };
        return with_last_good(source, identity, status, Some(why));
    }
    let Some(good) = &source.last_good else {
        return Plan {
            status: Status::Error,
            error: Some("no reading yet".to_owned()),
            ..identity
        };
    };
    let stale = good
        .fetched_at_ms
        .is_none_or(|at| now - at > STALE_AFTER_MS);
    with_last_good(
        source,
        identity,
        if stale { Status::Stale } else { Status::Ok },
        None,
    )
}

fn with_last_good(
    source: &SourceState,
    identity: Plan,
    status: Status,
    error: Option<String>,
) -> Plan {
    let (fetched_at_ms, windows) = source
        .last_good
        .as_ref()
        .map_or((None, Vec::new()), |good| {
            (good.fetched_at_ms, good.windows.clone())
        });
    Plan {
        status,
        error,
        fetched_at_ms,
        windows,
        ..identity
    }
}

/// A plan identity with no windows yet.
fn identity(provider: &str, harness: &str, source: &str) -> Plan {
    Plan {
        provider: provider.to_owned(),
        harness: harness.to_owned(),
        plan: None,
        label: None,
        account: None,
        source: source.to_owned(),
        status: Status::Ok,
        error: None,
        fetched_at_ms: None,
        windows: Vec::new(),
    }
}

/// Render the snapshot for a person.
#[must_use]
pub fn render_text(snapshot: &Snapshot, now_ms: i64) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    if snapshot.plans.is_empty() {
        out.push_str("no AI plans found on this host\n");
    }
    for plan in &snapshot.plans {
        let name = plan
            .label
            .as_deref()
            .or(plan.plan.as_deref())
            .unwrap_or("unknown plan");
        let _ = write!(out, "{} ({}) {name}", plan.provider, plan.harness);
        if plan.status != Status::Ok {
            let status = serde_json::to_value(plan.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let _ = write!(out, " [{status}]");
        }
        if let Some(error) = &plan.error {
            let _ = write!(out, " {error}");
        }
        out.push('\n');
        for window in &plan.windows {
            let _ = write!(out, "  {:<10} {:>5.1}%", window.label, window.used_percent);
            if let Some(reset) = window.resets_at_ms.filter(|at| *at > now_ms) {
                let _ = write!(out, "  resets in {}", util::human_wait(reset - now_ms));
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
pub(crate) fn test_plan() -> Plan {
    Plan {
        windows: vec![Window::by_length(300, 10.0, Some(1))],
        fetched_at_ms: Some(0),
        ..identity("claude", "claude-code", "oauth")
    }
}

#[cfg(test)]
type Recorded = Vec<(String, Vec<(String, String)>)>;

/// A scripted [`Fetch`] that records every request.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Script {
    replies: std::sync::Mutex<Vec<(String, Result<http::Reply, String>)>>,
    seen: std::sync::Mutex<Recorded>,
}

#[cfg(test)]
impl Script {
    pub(crate) fn reply(self, url_prefix: &str, status: u16, body: &str) -> Self {
        self.replies.lock().unwrap().push((
            url_prefix.to_owned(),
            Ok(http::Reply {
                status,
                body: body.as_bytes().to_vec(),
            }),
        ));
        self
    }

    pub(crate) fn requests(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(url, _)| url.clone())
            .collect()
    }

    pub(crate) fn header(&self, name: &str) -> Option<String> {
        self.seen.lock().unwrap().iter().find_map(|(_, headers)| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        })
    }
}

#[cfg(test)]
impl Fetch for Script {
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<http::Reply, String> {
        self.seen.lock().unwrap().push((
            url.to_owned(),
            headers
                .iter()
                .map(|(n, v)| ((*n).to_owned(), v.clone()))
                .collect(),
        ));
        let mut replies = self.replies.lock().unwrap();
        let at = replies
            .iter()
            .position(|(prefix, _)| url.starts_with(prefix.as_str()))
            .ok_or_else(|| format!("unscripted request to {url}"))?;
        replies.remove(at).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(fetch: &dyn Fetch, now_ms: i64, eager: bool) -> Request<'_> {
        Request {
            host: "studio",
            now_ms,
            eager,
            fetch,
        }
    }

    #[test]
    fn a_throttled_source_keeps_its_last_windows_as_stale() {
        let script = Script::default();
        let mut source = SourceState::default();
        let base = identity("claude", "claude-code", "oauth");
        let ok = polled(
            &mut source,
            base.clone(),
            request(&script, T0, false),
            || Ok(vec![Window::by_length(300, 50.0, None)]),
        );
        assert_eq!(ok.status, Status::Ok);

        let throttled = polled(
            &mut source,
            base.clone(),
            request(&script, T0 + MIN, true),
            || Err(Failure::Throttled),
        );
        assert_eq!(throttled.status, Status::Stale);
        assert_eq!(throttled.windows[0].used_percent, 50.0);
        assert_eq!(
            throttled.error.as_deref(),
            Some("rate limited, retry in 3m")
        );

        // Inside the backoff nothing is attempted, even eagerly.
        let held = polled(
            &mut source,
            base,
            request(&script, T0 + MIN + 60_000, true),
            || panic!("no request during a backoff"),
        );
        assert_eq!(held.error.as_deref(), Some("rate limited, retry in 2m"));
    }

    const MIN: i64 = state::MIN_INTERVAL_MS;
    const T0: i64 = 1_000;

    #[test]
    fn a_rejected_credential_is_unavailable_and_an_old_reading_goes_stale() {
        let script = Script::default();
        let mut source = SourceState::default();
        let base = identity("grok", "grok-build", "billing");
        let _ = polled(
            &mut source,
            base.clone(),
            request(&script, T0, false),
            || Ok(vec![Window::by_length(10_080, 5.0, None)]),
        );
        let quiet = polled(
            &mut source,
            base.clone(),
            request(&script, T0 + 1, false),
            || panic!("inside the minimum interval"),
        );
        assert_eq!(quiet.status, Status::Ok);
        let old = cached(&source, base.clone(), T0 + STALE_AFTER_MS + 1);
        assert_eq!(old.status, Status::Stale);

        let rejected = polled(
            &mut source,
            base.clone(),
            request(&script, T0 + MIN, false),
            || Err(Failure::from_status(401, "xAI")),
        );
        assert_eq!(rejected.status, Status::Unavailable);
        assert_eq!(rejected.windows.len(), 1, "the last reading still shows");
        let remembered = cached(&source, base, T0 + MIN + 1);
        assert_eq!(remembered.status, Status::Unavailable);
        assert_eq!(
            remembered.error.as_deref(),
            Some("xAI rejected the stored sign-in")
        );
    }

    #[test]
    fn a_host_with_no_vendor_files_publishes_no_plans() {
        let dir = tempfile::tempdir().unwrap();
        let sources = Sources {
            claude_dir: dir.path().join("claude"),
            claude_account: dir.path().join("claude.json"),
            keychain: false,
            codex_home: dir.path().join("codex"),
            grok_home: dir.path().join("grok"),
            grok_proxy: grok::DEFAULT_PROXY.to_owned(),
        };
        let script = Script::default();
        let mut state = State::default();
        let snapshot = collect(&sources, &mut state, request(&script, 5, false));
        assert_eq!(snapshot.schema, "phux.usage/v1");
        assert_eq!(snapshot.host, "studio");
        assert!(snapshot.plans.is_empty());
        assert!(script.requests().is_empty());
        assert_eq!(
            render_text(&snapshot, 5),
            "no AI plans found on this host\n"
        );
    }
}
