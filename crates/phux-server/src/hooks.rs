//! Server-side event-hook dispatcher (`docs/consumers/tui.md` §9, phux-r82.1).
//!
//! Two hook sources feed one dispatcher:
//!
//! * **Config hooks** — `[[hooks.<name>]]` entries from `config.toml`
//!   ([`phux_config::HookEntry`]): a `when` predicate plus an action.
//!   First match wins per event; only `run` actions (and the `noop`
//!   sentinel) are executable server-side — other action kinds (e.g.
//!   `message`) are client-side and skipped here.
//! * **Plugin event hooks** — enabled plugin manifests' `[[events]]`
//!   entries whose `on` names the event. Every matching plugin hook fires
//!   (the first-match-wins rule applies to config entries only).
//!
//! Execution is **child-process argv only** (the no-in-process-host rule),
//! via [`phux_plugin::run_command_spec`]: env injection, `kill_on_drop`,
//! and a per-hook timeout. Event context rides environment variables —
//! `PHUX_EVENT` plus one `PHUX_*` variable per context key, plus
//! `PHUX_SOCKET` (the server's listening UDS path, phux-d4rf) so a hook
//! script's bare `phux` invocation targets the firing server even when it
//! listens off the default socket path.
//!
//! # Threading
//!
//! Per ADR-0014 the server runs on a current-thread `LocalSet`.
//! [`HookDispatcher::fire`] is a synchronous, non-blocking `try_send` onto
//! a bounded queue: the terminal-actor hot path never awaits hook work,
//! and a full queue drops the event (hooks are an accelerator, never a
//! guarantee). The dispatcher task drains the queue and runs each matched
//! command on its own `spawn_local` task, gated by a semaphore so at most
//! [`MAX_CONCURRENT_HOOKS`] children run at once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use phux_config::plugin::{self, PluginPlatform};
use phux_config::{Action, Config, HookEntry, vocab};
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, warn};

/// Upper bound on concurrently-running hook child processes.
pub const MAX_CONCURRENT_HOOKS: usize = 8;

/// Bounded depth of the dispatcher's event queue. A full queue drops the
/// event (with a warning) rather than blocking the emitter.
pub const HOOK_EVENT_QUEUE: usize = 64;

/// Per-hook execution timeout. A hook still running when it expires is
/// killed and logged; it never wedges the dispatcher's concurrency budget.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// Hook point names (`docs/consumers/tui.md` §9). The definitions live
/// in [`phux_config::vocab`] (phux-i0e8.3.1) so `phux config check` can
/// validate `[[hooks.<event>]]` names; these re-exports keep the
/// server-side paths working.
pub use phux_config::vocab::{
    AFTER_NEW_PANE, AGENT_STATE_CHANGED, CLIENT_ATTACHED, CLIENT_DETACHED, FOCUS_CHANGED, PANE_EXIT,
};

/// The `to` value the agent hook reports when the detector withdraws a
/// record: the agent is gone, so its state is no longer knowable.
///
/// Matches the `unknown` word the L3 agent vocabulary already uses for a
/// withdrawn state, so a `when = { to = "unknown" }` clause reads the same
/// as the record a client would see.
pub const AGENT_STATE_UNKNOWN: &str = "unknown";

/// One fired hook event: a name from the §9 catalog plus its context.
///
/// Context keys use the same kebab-case vocabulary the config `when`
/// clauses match against (`exit-code`, `session`, ...); each key is also
/// exported to the hook child as `PHUX_<KEY>` (upper-cased, `-` → `_`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    /// Event name (e.g. [`PANE_EXIT`]).
    pub name: String,
    /// Kebab-case context keys and their string values.
    pub context: BTreeMap<String, String>,
}

impl HookEvent {
    /// Build an event from a name and context pairs.
    #[must_use]
    pub fn new(name: &str, context: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            name: name.to_owned(),
            context: context.into_iter().collect(),
        }
    }

    /// [`AFTER_NEW_PANE`]: fired right after a pane's actor spawns.
    #[must_use]
    pub fn after_new_pane(
        terminal_id: &phux_protocol::ids::TerminalId,
        session: Option<&str>,
    ) -> Self {
        let mut context = terminal_context(terminal_id);
        if let Some(session) = session {
            context.push(("session".to_owned(), session.to_owned()));
        }
        Self::new(AFTER_NEW_PANE, context)
    }

    /// [`PANE_EXIT`]: fired when a pane's inner process exits.
    /// `exit-code` is present only when the OS reported a code.
    #[must_use]
    pub fn pane_exit(
        terminal_id: &phux_protocol::ids::TerminalId,
        exit_status: Option<i32>,
    ) -> Self {
        let mut context = terminal_context(terminal_id);
        if let Some(code) = exit_status {
            context.push(("exit-code".to_owned(), code.to_string()));
        }
        Self::new(PANE_EXIT, context)
    }

    /// [`FOCUS_CHANGED`]: fired when a client's focus lands on a pane
    /// (an `INPUT_FOCUS` gained event that passed the routing gates).
    #[must_use]
    pub fn focus_changed(
        terminal_id: &phux_protocol::ids::TerminalId,
        client_id: crate::state::ClientId,
    ) -> Self {
        let mut context = terminal_context(terminal_id);
        context.push(("client-id".to_owned(), client_id.0.to_string()));
        Self::new(FOCUS_CHANGED, context)
    }

    /// [`CLIENT_ATTACHED`]: fired after a client's ATTACH completes.
    #[must_use]
    pub fn client_attached(client_id: crate::state::ClientId, session: &str) -> Self {
        Self::new(
            CLIENT_ATTACHED,
            [
                ("client-id".to_owned(), client_id.0.to_string()),
                ("session".to_owned(), session.to_owned()),
            ],
        )
    }

    /// [`CLIENT_DETACHED`]: fired when an attached client detaches for any
    /// reason (explicit DETACH, transport drop). `session` may be absent if
    /// the session was reaped before the detach ran.
    #[must_use]
    pub fn client_detached(client_id: crate::state::ClientId, session: Option<&str>) -> Self {
        let mut context = vec![("client-id".to_owned(), client_id.0.to_string())];
        if let Some(session) = session {
            context.push(("session".to_owned(), session.to_owned()));
        }
        Self::new(CLIENT_DETACHED, context)
    }

    /// [`AGENT_STATE_CHANGED`]: fired when the detector's published agent
    /// state for a pane actually changes (ADR-0046).
    ///
    /// This is the notification seam. phux ships no sound player and no
    /// desktop-notification client — a `run` action reaching whatever the
    /// host already has (`osascript`, `notify-send`, `afplay`, `tput bel`)
    /// composes better than a built-in that must be configured to be
    /// silenced. What the server owes the operator is the *edge*, delivered
    /// once, with enough context to decide.
    ///
    /// `from` is absent when the pane had no prior record, which is how a
    /// first sighting is distinguished from a transition. Both `from` and
    /// `to` use the L3 agent-state vocabulary.
    #[must_use]
    pub fn agent_state_changed(
        terminal_id: &phux_protocol::ids::TerminalId,
        kind: &str,
        name: &str,
        from: Option<&str>,
        to: &str,
    ) -> Self {
        let mut context = terminal_context(terminal_id);
        context.push(("agent-kind".to_owned(), kind.to_owned()));
        if !name.is_empty() {
            context.push(("agent-name".to_owned(), name.to_owned()));
        }
        if let Some(from) = from {
            context.push(("from".to_owned(), from.to_owned()));
        }
        context.push(("to".to_owned(), to.to_owned()));
        Self::new(AGENT_STATE_CHANGED, context)
    }
}

/// Shared context helper: the pane's wire-local id, when it has one.
fn terminal_context(terminal_id: &phux_protocol::ids::TerminalId) -> Vec<(String, String)> {
    terminal_id
        .local_id()
        .map(|id| ("terminal-id".to_owned(), id.to_string()))
        .into_iter()
        .collect()
}

/// One `[[events]]` entry resolved from an **enabled** plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEventHook {
    /// Manifest-global plugin id.
    pub plugin_id: String,
    /// Plugin-local event hook id.
    pub event_id: String,
    /// Event name this hook observes (matched against [`HookEvent::name`]).
    pub on: String,
    /// Command argv to execute.
    pub command: Vec<String>,
    /// Directory containing the manifest; the hook's working directory.
    pub plugin_root: PathBuf,
}

/// Everything the dispatcher matches events against: config `[[hooks.*]]`
/// entries plus the event hooks of every enabled plugin manifest.
#[derive(Debug, Clone, Default)]
pub struct HookCatalog {
    /// `[[hooks.<name>]]` entries keyed by hook name.
    pub config_hooks: BTreeMap<String, Vec<HookEntry>>,
    /// Event hooks resolved from enabled plugin manifests.
    pub plugin_events: Vec<PluginEventHook>,
}

impl HookCatalog {
    /// `true` when there is nothing to dispatch — the runtime skips
    /// spawning the dispatcher task entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.config_hooks.is_empty() && self.plugin_events.is_empty()
    }

    /// Build a catalog from a loaded config.
    ///
    /// `config_path` anchors relative plugin-manifest paths (they resolve
    /// against the config file's directory, matching the phux-plugin action
    /// runtime). Disabled plugins are skipped, as are `[[events]]` entries
    /// whose `platforms` list excludes the current OS. A manifest that
    /// fails to load is logged and skipped rather than failing the server.
    ///
    /// Config hook entries that can never do what they say — unknown
    /// event names, `when` keys outside the event's context, actions
    /// that never execute server-side — are each warned about once here
    /// (phux-i0e8.3.3), so a config that never saw `phux config check`
    /// is still loud at startup. The entries are kept, not dropped:
    /// first-match-wins semantics are unchanged.
    #[must_use]
    pub fn from_config(cfg: &Config, config_path: &Path) -> Self {
        for problem in offending_config_hooks(&cfg.hooks) {
            warn!(
                hook = %problem.label,
                "hooks: {}; run `phux config check`",
                problem.detail,
            );
        }
        let mut plugin_events = Vec::new();
        for entry in &cfg.plugins {
            if !entry.enabled {
                debug!(manifest = %entry.manifest.display(), "hooks: plugin disabled; skipping its events");
                continue;
            }
            let manifest_path = resolve_manifest_path(&entry.manifest, config_path);
            let manifest = match plugin::load_plugin_manifest(&manifest_path) {
                Ok(manifest) => manifest,
                Err(err) => {
                    warn!(
                        manifest = %manifest_path.display(),
                        error = %err,
                        "hooks: could not load plugin manifest; skipping its events",
                    );
                    continue;
                }
            };
            for event in manifest.events {
                if !platform_enabled(event.platforms.as_deref()) {
                    continue;
                }
                plugin_events.push(PluginEventHook {
                    plugin_id: manifest.id.clone(),
                    event_id: event.id,
                    on: event.on,
                    command: event.command,
                    plugin_root: manifest.plugin_root.clone(),
                });
            }
        }
        Self {
            config_hooks: cfg.hooks.clone(),
            plugin_events,
        }
    }
}

/// One config hook table or entry that can never do what it says: a
/// log-ready label (`hooks.pane-exit[1]`, matching the dispatch labels)
/// plus what is wrong with it.
#[derive(Debug, PartialEq, Eq)]
struct HookConfigProblem {
    label: String,
    detail: String,
}

/// Find every config hook entry `phux config check` would flag
/// (phux-i0e8.3.3), one problem record per offending entry.
///
/// Same vocabulary the check's semantic pass uses
/// ([`phux_config::vocab`]), so startup warnings and check findings
/// cannot disagree: an unknown event name yields one record for the
/// whole (dead) table; a known event's entries each yield at most one
/// record aggregating their unknown `when` keys (checked after
/// stripping the `-startswith` suffix) and never-executable action
/// (`noop` is the deliberate sentinel, not a mistake).
fn offending_config_hooks(hooks: &BTreeMap<String, Vec<HookEntry>>) -> Vec<HookConfigProblem> {
    let mut problems = Vec::new();
    for (event, entries) in hooks {
        let Some(context_keys) = vocab::hook_context_keys(event) else {
            let suggestion = vocab::did_you_mean(event, vocab::HOOK_EVENTS)
                .map(|hit| format!(" (did you mean `{hit}`?)"))
                .unwrap_or_default();
            problems.push(HookConfigProblem {
                label: format!("hooks.{event}"),
                detail: format!("unknown event `{event}`{suggestion}; its entries will never fire"),
            });
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let mut details = Vec::new();
            for key in entry.when.keys() {
                let base = key.strip_suffix("-startswith").unwrap_or(key);
                if !context_keys.contains(&base) {
                    details.push(format!(
                        "when key `{key}` can never match (`{event}` context keys: {})",
                        context_keys.join(", "),
                    ));
                }
            }
            let name = config_action_name(&entry.action);
            if name != "noop" && !vocab::hook_action_is_executable(&entry.action) {
                details.push(format!(
                    "action `{name}` never executes server-side (only `run` with a usable \
                     `command` does); a match still consumes the event"
                ));
            }
            if !details.is_empty() {
                problems.push(HookConfigProblem {
                    label: format!("hooks.{event}[{index}]"),
                    detail: details.join("; "),
                });
            }
        }
    }
    problems
}

/// The action name a config hook entry invokes, whichever spelling
/// (`action = "noop"` or `{ kind = "run", ... }`) it used.
fn config_action_name(action: &Action) -> &str {
    match action {
        Action::Bare(name) => name,
        Action::Parameterized(parameterized) => &parameterized.action,
    }
}

/// Resolve a plugin manifest path: absolute paths pass through; relative
/// paths anchor at the config file's directory.
fn resolve_manifest_path(manifest: &Path, config_path: &Path) -> PathBuf {
    if manifest.is_absolute() {
        return manifest.to_path_buf();
    }
    config_path
        .parent()
        .map_or_else(|| manifest.to_path_buf(), |parent| parent.join(manifest))
}

/// `true` when the current OS is allowed by a manifest `platforms` list
/// (`None` = every platform).
fn platform_enabled(platforms: Option<&[PluginPlatform]>) -> bool {
    let Some(platforms) = platforms else {
        return true;
    };
    current_platform().is_some_and(|current| platforms.contains(&current))
}

/// The manifest-vocabulary name of the OS this server runs on.
const fn current_platform() -> Option<PluginPlatform> {
    if cfg!(target_os = "macos") {
        Some(PluginPlatform::Macos)
    } else if cfg!(target_os = "linux") {
        Some(PluginPlatform::Linux)
    } else if cfg!(target_os = "windows") {
        Some(PluginPlatform::Windows)
    } else {
        None
    }
}

/// Cheap, cloneable handle for firing events at the dispatcher task.
#[derive(Debug, Clone)]
pub struct HookDispatcher {
    tx: mpsc::Sender<HookEvent>,
}

impl HookDispatcher {
    /// Queue `event` for dispatch. **Never blocks**: this is a bounded
    /// `try_send`; when the queue is full (or the dispatcher is gone) the
    /// event is dropped with a log line. Safe to call from any server task,
    /// but not while holding the [`crate::state::SharedState`] lock — see
    /// the crate-internal `fire_hook` helper.
    pub fn fire(&self, event: HookEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(event)) => {
                warn!(event = %event.name, "hook queue full; dropping event");
            }
            Err(mpsc::error::TrySendError::Closed(event)) => {
                debug!(event = %event.name, "hook dispatcher gone; dropping event");
            }
        }
    }

    /// Test-only constructor wrapping a raw queue so call-site wiring can
    /// be observed without spawning the dispatcher task.
    #[cfg(test)]
    pub(crate) const fn from_sender(tx: mpsc::Sender<HookEvent>) -> Self {
        Self { tx }
    }
}

/// Fire `event` through the dispatcher registered on `state`, if any.
///
/// Synchronous and non-blocking (see [`HookDispatcher::fire`]). Takes the
/// state lock briefly to clone the handle, so it MUST NOT be called from
/// inside a `with` / `with_mut` closure (the mutex is not reentrant).
pub(crate) fn fire_hook(state: &crate::state::SharedState, event: HookEvent) {
    let Some(dispatcher) = state.with(|s| s.hook_dispatcher().cloned()) else {
        return;
    };
    dispatcher.fire(event);
}

/// Spawn the dispatcher task on the current `LocalSet` and return the
/// fire handle.
///
/// The task drains the bounded event queue; each matched hook command runs
/// on its own `spawn_local` task behind a [`MAX_CONCURRENT_HOOKS`]-wide
/// semaphore, with [`HOOK_TIMEOUT`] and `kill_on_drop` (via
/// [`phux_plugin::run_command_spec`]). The task exits when every
/// [`HookDispatcher`] clone is dropped.
///
/// `server_socket` is the UDS path this server listens on, injected into
/// every hook child as `PHUX_SOCKET` (phux-d4rf) — the hook analogue of
/// [`crate::terminal_actor::apply_server_socket`] (phux-cufw). `None`
/// (test-only wiring) leaves the environment untouched, preserving any
/// `PHUX_SOCKET` the daemon itself inherited.
#[must_use]
pub fn spawn_hook_dispatcher(
    catalog: HookCatalog,
    server_socket: Option<PathBuf>,
) -> HookDispatcher {
    let (tx, mut rx) = mpsc::channel::<HookEvent>(HOOK_EVENT_QUEUE);
    tokio::task::spawn_local(async move {
        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_HOOKS));
        while let Some(event) = rx.recv().await {
            for run in matched_runs(&catalog, &event, server_socket.as_deref()) {
                let semaphore = Arc::clone(&semaphore);
                tokio::task::spawn_local(async move {
                    // `acquire_owned` fails only if the semaphore is closed,
                    // which never happens here (we never call `close`).
                    let Ok(_permit) = semaphore.acquire_owned().await else {
                        return;
                    };
                    execute(run).await;
                });
            }
        }
        debug!("hook dispatcher exiting (all handles dropped)");
    });
    HookDispatcher { tx }
}

/// One resolved hook execution: a label for logs plus the command spec.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HookRun {
    label: String,
    spec: phux_plugin::CommandSpec,
}

/// Resolve every command `event` should fire: the first matching config
/// entry (first-match-wins per §9) plus every plugin event hook whose `on`
/// names the event.
fn matched_runs(
    catalog: &HookCatalog,
    event: &HookEvent,
    server_socket: Option<&Path>,
) -> Vec<HookRun> {
    let env = event_env(event, server_socket);
    let mut runs = Vec::new();

    if let Some(entries) = catalog.config_hooks.get(&event.name) {
        for (index, entry) in entries.iter().enumerate() {
            if !when_matches(&entry.when, &event.context) {
                continue;
            }
            // First match wins: this entry consumes the event whether or
            // not its action is executable server-side.
            if let Some(argv) = action_argv(&entry.action) {
                runs.push(HookRun {
                    label: format!("hooks.{}[{index}]", event.name),
                    spec: phux_plugin::CommandSpec {
                        argv,
                        cwd: None,
                        env: env.clone(),
                        timeout: Some(HOOK_TIMEOUT),
                    },
                });
            } else {
                warn!(
                    event = %event.name,
                    index,
                    "hook entry matched but its action is not server-executable; \
                     the event is consumed and nothing runs -- run `phux config check`",
                );
            }
            break;
        }
    }

    for hook in catalog
        .plugin_events
        .iter()
        .filter(|hook| hook.on == event.name)
    {
        let mut plugin_env = env.clone();
        plugin_env.push(("PHUX_PLUGIN_ID".to_owned(), hook.plugin_id.clone()));
        plugin_env.push(("PHUX_PLUGIN_EVENT_ID".to_owned(), hook.event_id.clone()));
        plugin_env.push((
            "PHUX_PLUGIN_ROOT".to_owned(),
            hook.plugin_root.display().to_string(),
        ));
        runs.push(HookRun {
            label: format!("plugin.{}.{}", hook.plugin_id, hook.event_id),
            spec: phux_plugin::CommandSpec {
                argv: hook.command.clone(),
                cwd: Some(hook.plugin_root.clone()),
                env: plugin_env,
                timeout: Some(HOOK_TIMEOUT),
            },
        });
    }

    runs
}

/// The environment injected into every hook child: `PHUX_EVENT` plus one
/// `PHUX_<KEY>` entry per context key (`-` → `_`, upper-cased).
///
/// When `server_socket` is known, `PHUX_SOCKET` is injected too (phux-d4rf),
/// mirroring [`crate::terminal_actor::apply_server_socket`]'s `Option`
/// handling: absent means the environment is left untouched.
fn event_env(event: &HookEvent, server_socket: Option<&Path>) -> Vec<(String, String)> {
    let mut env = vec![("PHUX_EVENT".to_owned(), event.name.clone())];
    for (key, value) in &event.context {
        env.push((env_var_name(key), value.clone()));
    }
    if let Some(path) = server_socket {
        env.push(("PHUX_SOCKET".to_owned(), path.display().to_string()));
    }
    env
}

/// `exit-code` → `PHUX_EXIT_CODE`.
fn env_var_name(key: &str) -> String {
    let mut name = String::with_capacity(key.len() + 5);
    name.push_str("PHUX_");
    for ch in key.chars() {
        name.push(if ch == '-' {
            '_'
        } else {
            ch.to_ascii_uppercase()
        });
    }
    name
}

/// Evaluate a config entry's `when` clauses against the event context.
///
/// All clauses must hold (AND). Per §9 the language is deliberately tiny:
///
/// * `"*"` matches unconditionally.
/// * A key ending in `-startswith` prefix-matches the base context key
///   (`cwd-startswith = "/x"` matches context `cwd = "/x/y"`).
/// * Anything else is an exact string match against the context value
///   (non-string TOML scalars compare via their canonical rendering, so
///   `exit-code = 0` matches context `exit-code = "0"`).
fn when_matches(when: &BTreeMap<String, toml::Value>, context: &BTreeMap<String, String>) -> bool {
    when.iter()
        .all(|(key, expected)| clause_matches(key, expected, context))
}

/// Evaluate one `when` clause (see [`when_matches`]).
fn clause_matches(key: &str, expected: &toml::Value, context: &BTreeMap<String, String>) -> bool {
    let expected = toml_scalar_string(expected);
    if expected == "*" {
        return true;
    }
    if let Some(base) = key.strip_suffix("-startswith") {
        return context
            .get(base)
            .is_some_and(|value| value.starts_with(&expected));
    }
    context.get(key).is_some_and(|value| *value == expected)
}

/// Render a TOML scalar the way the context strings are rendered
/// (`0` → `"0"`, `true` → `"true"`, strings verbatim).
fn toml_scalar_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Resolve a config hook action to a child-process argv.
///
/// Only `run` is executable server-side: `command` may be a string
/// (executed via `/bin/sh -c`, still a child process — the
/// no-in-process-host rule is about hosting plugin code, not about using
/// the shell as the argv) or an array of argv strings. `noop` and every
/// other action kind (`message`, ... — client-side by design) yield `None`.
fn action_argv(action: &Action) -> Option<Vec<String>> {
    let parameterized = match action {
        Action::Bare(_) => return None,
        Action::Parameterized(p) => p,
    };
    if parameterized.action != "run" {
        return None;
    }
    match parameterized.args.get("command") {
        Some(toml::Value::String(command)) if !command.trim().is_empty() => {
            Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), command.clone()])
        }
        Some(toml::Value::Array(items)) => {
            let argv: Vec<String> = items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect();
            (argv.len() == items.len() && !argv.is_empty()).then_some(argv)
        }
        _ => {
            warn!("hook `run` action has no usable `command`; skipping");
            None
        }
    }
}

/// Run one hook child to completion and log the outcome.
async fn execute(run: HookRun) {
    let label = run.label;
    match phux_plugin::run_command_spec(run.spec).await {
        Ok(output) if output.outcome == phux_plugin::PluginActionOutcome::TimedOut => {
            warn!(hook = %label, timeout = ?HOOK_TIMEOUT, "hook timed out; child killed");
        }
        Ok(output) if output.exit_code != Some(0) => {
            warn!(
                hook = %label,
                exit_code = ?output.exit_code,
                stderr = %output.stderr,
                "hook exited non-zero",
            );
        }
        Ok(output) => {
            debug!(hook = %label, duration_ms = output.duration_ms, "hook completed");
        }
        Err(err) => {
            warn!(hook = %label, error = %err, "hook failed to spawn");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn ctx(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn when(toml_inline: &str) -> BTreeMap<String, toml::Value> {
        toml::from_str(toml_inline).expect("valid when table")
    }

    #[test]
    fn when_empty_matches_everything() {
        assert!(when_matches(&BTreeMap::new(), &ctx(&[])));
        assert!(when_matches(&BTreeMap::new(), &ctx(&[("exit-code", "1")])));
    }

    #[test]
    fn when_exact_string_and_integer_match_context() {
        let w = when("exit-code = 0");
        assert!(when_matches(&w, &ctx(&[("exit-code", "0")])));
        assert!(!when_matches(&w, &ctx(&[("exit-code", "1")])));
        // Missing key never matches an exact clause.
        assert!(!when_matches(&w, &ctx(&[])));
        let w = when("session = \"work\"");
        assert!(when_matches(&w, &ctx(&[("session", "work")])));
        assert!(!when_matches(&w, &ctx(&[("session", "home")])));
    }

    #[test]
    fn when_star_matches_even_absent_keys() {
        let w = when("exit-code = \"*\"");
        assert!(when_matches(&w, &ctx(&[("exit-code", "137")])));
        // Signal-killed child: no exit code in context. `"*"` still fires.
        assert!(when_matches(&w, &ctx(&[])));
    }

    #[test]
    fn when_startswith_prefix_matches_base_key() {
        let w = when("cwd-startswith = \"/Users/x/work\"");
        assert!(when_matches(&w, &ctx(&[("cwd", "/Users/x/work/repo")])));
        assert!(!when_matches(&w, &ctx(&[("cwd", "/tmp")])));
        assert!(!when_matches(&w, &ctx(&[])));
    }

    #[test]
    fn when_multiple_clauses_are_anded() {
        let w = when("exit-code = 0\nsession = \"work\"");
        assert!(when_matches(
            &w,
            &ctx(&[("exit-code", "0"), ("session", "work")])
        ));
        assert!(!when_matches(
            &w,
            &ctx(&[("exit-code", "0"), ("session", "home")])
        ));
    }

    fn action(toml_inline: &str) -> Action {
        #[derive(serde::Deserialize)]
        struct Holder {
            action: Action,
        }
        let holder: Holder = toml::from_str(toml_inline).expect("valid action");
        holder.action
    }

    #[test]
    fn action_argv_shapes() {
        // Bare `noop` (and any bare string) is not server-executable.
        assert_eq!(action_argv(&action("action = \"noop\"")), None);
        // `run` with a string command goes through /bin/sh -c.
        assert_eq!(
            action_argv(&action(
                "action = { kind = \"run\", command = \"echo hi\" }"
            )),
            Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "echo hi".to_owned()
            ])
        );
        // `run` with an argv array executes directly.
        assert_eq!(
            action_argv(&action(
                "action = { kind = \"run\", command = [\"say\", \"done\"] }"
            )),
            Some(vec!["say".to_owned(), "done".to_owned()])
        );
        // Client-side kinds are skipped.
        assert_eq!(
            action_argv(&action(
                "action = { kind = \"message\", text = \"in work tree\" }"
            )),
            None
        );
        // A run action with a malformed command is skipped.
        assert_eq!(
            action_argv(&action("action = { kind = \"run\", command = 3 }")),
            None
        );
        assert_eq!(
            action_argv(&action("action = { kind = \"run\", command = [] }")),
            None
        );
    }

    /// Agreement test (phux-i0e8.3.3): the pure predicate
    /// `phux_config::vocab::hook_action_is_executable` must agree with
    /// [`action_argv`], the dispatcher's actual resolver, on every
    /// action shape — otherwise `phux config check` would bless hooks
    /// the server skips, or flag hooks that run fine.
    #[test]
    fn executability_predicate_agrees_with_action_argv() {
        let cases = [
            "action = \"noop\"",
            "action = \"kill-pane\"",
            "action = { kind = \"noop\" }",
            "action = { kind = \"message\", text = \"hi\" }",
            "action = { kind = \"run\", command = \"echo hi\" }",
            "action = { kind = \"run\", command = \"   \" }",
            "action = { kind = \"run\", command = [\"say\", \"done\"] }",
            "action = { kind = \"run\", command = [] }",
            "action = { kind = \"run\", command = [\"say\", 3] }",
            "action = { kind = \"run\", command = 3 }",
            "action = { kind = \"run\" }",
        ];
        for case in cases {
            let action = action(case);
            assert_eq!(
                vocab::hook_action_is_executable(&action),
                action_argv(&action).is_some(),
                "predicate disagrees with action_argv for: {case}",
            );
        }
    }

    /// Agreement test (phux-i0e8.3.3): `vocab::hook_context_keys` is
    /// documented code-is-truth against these constructors. Build each
    /// event with every optional key present and assert the key sets
    /// match exactly — a constructor growing a key without teaching the
    /// vocab (or vice versa) fails here, not in a user's silent hook.
    #[test]
    fn vocab_context_keys_match_the_event_constructors() {
        let terminal = phux_protocol::ids::TerminalId::local(7);
        let client = crate::state::ClientId(3);
        let events = [
            HookEvent::after_new_pane(&terminal, Some("work")),
            HookEvent::pane_exit(&terminal, Some(0)),
            HookEvent::focus_changed(&terminal, client),
            HookEvent::client_attached(client, "work"),
            HookEvent::client_detached(client, Some("work")),
            HookEvent::agent_state_changed(&terminal, "claude", "reviewer", Some("busy"), "idle"),
        ];
        assert_eq!(
            events.len(),
            vocab::HOOK_EVENTS.len(),
            "an event is missing from this agreement test",
        );
        for event in events {
            let expected = vocab::hook_context_keys(&event.name)
                .unwrap_or_else(|| panic!("constructor built unknown event `{}`", event.name));
            let got: Vec<&str> = event.context.keys().map(String::as_str).collect();
            assert_eq!(got, expected, "context keys drifted for `{}`", event.name);
        }
    }

    /// Startup validation (phux-i0e8.3.3): each offending config entry
    /// yields exactly one problem record — an unknown event covers its
    /// whole table, an entry's unknown when key and dead action
    /// aggregate — and a clean config yields none. `from_config` warns
    /// once per record.
    #[test]
    fn offending_config_hooks_flags_each_bad_entry_once() {
        let cfg: Config = toml::from_str(
            r#"
            [[hooks.pane-exited]]
            action = "noop"

            [[hooks.pane-exit]]
            when = { exit-code = 0 }
            action = "noop"

            [[hooks.pane-exit]]
            when = { exitcode = "*" }
            action = { kind = "message", text = "bye" }

            [[hooks.after-new-pane]]
            when = { session-startswith = "work" }
            action = { kind = "run", command = "true" }
            "#,
        )
        .expect("valid config");
        let problems = offending_config_hooks(&cfg.hooks);
        let labels: Vec<&str> = problems.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["hooks.pane-exit[1]", "hooks.pane-exited"],
            "problems: {problems:?}",
        );
        let entry_problem = &problems[0].detail;
        assert!(
            entry_problem.contains("when key `exitcode` can never match")
                && entry_problem.contains("action `message` never executes server-side"),
            "entry problems not aggregated: {entry_problem}",
        );
        assert!(
            problems[1].detail.contains("did you mean `pane-exit`?"),
            "no suggestion in: {}",
            problems[1].detail,
        );
    }

    #[test]
    fn env_var_naming_uppercases_and_underscores() {
        assert_eq!(env_var_name("exit-code"), "PHUX_EXIT_CODE");
        assert_eq!(env_var_name("terminal-id"), "PHUX_TERMINAL_ID");
        assert_eq!(env_var_name("session"), "PHUX_SESSION");
    }

    fn catalog_from_toml(hooks_toml: &str) -> HookCatalog {
        let cfg: Config = toml::from_str(hooks_toml).expect("valid config");
        HookCatalog {
            config_hooks: cfg.hooks,
            plugin_events: Vec::new(),
        }
    }

    #[test]
    fn first_matching_config_entry_wins_and_consumes_the_event() {
        // Entry 0 (noop) matches exit-code 0 and consumes the event, so
        // the catch-all `run` in entry 1 must NOT fire.
        let catalog = catalog_from_toml(
            r#"
            [[hooks.pane-exit]]
            when = { exit-code = 0 }
            action = "noop"

            [[hooks.pane-exit]]
            when = { exit-code = "*" }
            action = { kind = "run", command = "echo boom" }
            "#,
        );
        let clean = HookEvent::new(PANE_EXIT, [("exit-code".to_owned(), "0".to_owned())]);
        assert!(matched_runs(&catalog, &clean, None).is_empty());

        // A non-zero exit skips entry 0 and lands on the catch-all.
        let dirty = HookEvent::new(PANE_EXIT, [("exit-code".to_owned(), "1".to_owned())]);
        let runs = matched_runs(&catalog, &dirty, None);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].label, "hooks.pane-exit[1]");
    }

    #[test]
    fn matched_runs_injects_event_env() {
        let catalog = catalog_from_toml(
            r#"
            [[hooks.pane-exit]]
            action = { kind = "run", command = "true" }
            "#,
        );
        let event = HookEvent::new(
            PANE_EXIT,
            [
                ("exit-code".to_owned(), "0".to_owned()),
                ("terminal-id".to_owned(), "7".to_owned()),
            ],
        );
        let runs = matched_runs(&catalog, &event, None);
        assert_eq!(runs.len(), 1);
        let env = &runs[0].spec.env;
        assert!(env.contains(&("PHUX_EVENT".to_owned(), "pane-exit".to_owned())));
        assert!(env.contains(&("PHUX_EXIT_CODE".to_owned(), "0".to_owned())));
        assert!(env.contains(&("PHUX_TERMINAL_ID".to_owned(), "7".to_owned())));
        // No server socket configured: the env carries no PHUX_SOCKET at
        // all, preserving anything the daemon itself inherited (phux-d4rf).
        assert!(env.iter().all(|(key, _)| key != "PHUX_SOCKET"));
    }

    #[test]
    fn matched_runs_injects_server_socket_when_known() {
        let catalog = catalog_from_toml(
            r#"
            [[hooks.pane-exit]]
            action = { kind = "run", command = "true" }
            "#,
        );
        let event = HookEvent::new(PANE_EXIT, []);
        let socket = Path::new("/tmp/phux-test/alt.sock");
        let runs = matched_runs(&catalog, &event, Some(socket));
        assert_eq!(runs.len(), 1);
        assert!(runs[0].spec.env.contains(&(
            "PHUX_SOCKET".to_owned(),
            "/tmp/phux-test/alt.sock".to_owned()
        )));
    }

    #[test]
    fn plugin_events_match_on_name_only_and_all_fire() {
        let hook = |on: &str, id: &str| PluginEventHook {
            plugin_id: "p".to_owned(),
            event_id: id.to_owned(),
            on: on.to_owned(),
            command: vec!["true".to_owned()],
            plugin_root: PathBuf::from("/tmp"),
        };
        let catalog = HookCatalog {
            config_hooks: BTreeMap::new(),
            plugin_events: vec![
                hook(AFTER_NEW_PANE, "a"),
                hook(PANE_EXIT, "b"),
                hook(AFTER_NEW_PANE, "c"),
            ],
        };
        let event = HookEvent::new(AFTER_NEW_PANE, []);
        let runs = matched_runs(&catalog, &event, None);
        assert_eq!(runs.len(), 2, "both after-new-pane plugin hooks fire");
        assert!(runs.iter().all(|run| {
            run.spec
                .env
                .contains(&("PHUX_PLUGIN_ID".to_owned(), "p".to_owned()))
        }));
        assert_eq!(runs[0].spec.cwd.as_deref(), Some(Path::new("/tmp")));
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn catalog_skips_disabled_plugins_and_keeps_enabled_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = |id: &str| {
            format!(
                r#"
                id = "{id}"
                name = "Test"
                version = "0.1.0"
                min_phux_version = "0.0.1"

                [[events]]
                id = "greet"
                title = "Greet"
                on = "after-new-pane"
                command = ["true"]
                "#
            )
        };
        write(
            &dir.path().join("plugins/on/phux-plugin.toml"),
            &manifest("plugin-on"),
        );
        write(
            &dir.path().join("plugins/off/phux-plugin.toml"),
            &manifest("plugin-off"),
        );
        let cfg: Config = toml::from_str(
            r#"
            [[plugins]]
            manifest = "plugins/on/phux-plugin.toml"

            [[plugins]]
            manifest = "plugins/off/phux-plugin.toml"
            enabled = false
            "#,
        )
        .expect("valid config");

        let catalog = HookCatalog::from_config(&cfg, &dir.path().join("config.toml"));
        assert_eq!(catalog.plugin_events.len(), 1);
        assert_eq!(catalog.plugin_events[0].plugin_id, "plugin-on");
        assert_eq!(catalog.plugin_events[0].on, AFTER_NEW_PANE);

        // Dispatcher-level proof: a disabled plugin's event never fires
        // because it is simply not in the catalog.
        let event = HookEvent::new(AFTER_NEW_PANE, []);
        let runs = matched_runs(&catalog, &event, None);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].label, "plugin.plugin-on.greet");
    }

    #[test]
    fn catalog_skips_manifest_load_failures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg: Config = toml::from_str(
            r#"
            [[plugins]]
            manifest = "plugins/missing/phux-plugin.toml"
            "#,
        )
        .expect("valid config");
        let catalog = HookCatalog::from_config(&cfg, &dir.path().join("config.toml"));
        assert!(catalog.plugin_events.is_empty());
        assert!(catalog.is_empty());
    }

    #[test]
    fn fire_on_full_or_closed_queue_never_blocks_or_panics() {
        // Full queue: capacity 1, nothing draining. The second fire drops.
        let (tx, rx) = mpsc::channel::<HookEvent>(1);
        let dispatcher = HookDispatcher::from_sender(tx);
        let started = Instant::now();
        dispatcher.fire(HookEvent::new(PANE_EXIT, []));
        dispatcher.fire(HookEvent::new(PANE_EXIT, []));
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "fire must be non-blocking even when the queue is full",
        );
        // Closed queue: receiver gone. Fire is a logged no-op.
        drop(rx);
        dispatcher.fire(HookEvent::new(PANE_EXIT, []));
    }

    #[test]
    fn fire_hook_without_registered_dispatcher_is_noop() {
        let state = crate::state::SharedState::new();
        fire_hook(&state, HookEvent::new(PANE_EXIT, []));
    }

    /// End-to-end through the real dispatcher task: a config `run` hook
    /// executes as a child process with the event env injected, including
    /// the server's socket path as `PHUX_SOCKET` (phux-d4rf).
    #[tokio::test(flavor = "current_thread")]
    async fn dispatcher_executes_config_hook_with_env_injection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("marker");
        let socket = dir.path().join("phux.sock");
        let command = format!(
            "printf '%s %s %s' \"$PHUX_EVENT\" \"$PHUX_TERMINAL_ID\" \"$PHUX_SOCKET\" > {}",
            marker.display()
        );
        // Built programmatically: the shell command mixes quote styles
        // that are painful to embed in a TOML literal.
        let entry = HookEntry {
            when: BTreeMap::new(),
            action: Action::Parameterized(phux_config::ParamAction {
                action: "run".to_owned(),
                args: std::iter::once(("command".to_owned(), toml::Value::String(command)))
                    .collect(),
            }),
        };
        let catalog = HookCatalog {
            config_hooks: std::iter::once((AFTER_NEW_PANE.to_owned(), vec![entry])).collect(),
            plugin_events: Vec::new(),
        };

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dispatcher = spawn_hook_dispatcher(catalog, Some(socket.clone()));
                dispatcher.fire(HookEvent::new(
                    AFTER_NEW_PANE,
                    [("terminal-id".to_owned(), "42".to_owned())],
                ));
                wait_for_file(&marker).await;
            })
            .await;
        let contents = std::fs::read_to_string(&marker).expect("marker written");
        assert_eq!(contents, format!("after-new-pane 42 {}", socket.display()));
    }

    /// A plugin `[[events]]` hook runs with the plugin root as cwd and the
    /// plugin identity env vars alongside the event env.
    #[tokio::test(flavor = "current_thread")]
    async fn dispatcher_executes_plugin_event_in_plugin_root_with_plugin_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");
        let catalog = HookCatalog {
            config_hooks: BTreeMap::new(),
            plugin_events: vec![PluginEventHook {
                plugin_id: "notifier".to_owned(),
                event_id: "on-exit".to_owned(),
                on: PANE_EXIT.to_owned(),
                command: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "printf '%s %s %s' \"$PHUX_PLUGIN_ID\" \"$PHUX_EXIT_CODE\" \"$PWD\" > marker"
                        .to_owned(),
                ],
                plugin_root: root.clone(),
            }],
        };
        let marker = root.join("marker");

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dispatcher = spawn_hook_dispatcher(catalog, None);
                dispatcher.fire(HookEvent::new(
                    PANE_EXIT,
                    [("exit-code".to_owned(), "0".to_owned())],
                ));
                wait_for_file(&marker).await;
            })
            .await;
        let contents = std::fs::read_to_string(&marker).expect("marker written");
        assert_eq!(contents, format!("notifier 0 {}", root.display()));
    }

    /// A slow hook must not block `fire` (the emitter's contract) — the
    /// child runs on its own task behind the semaphore.
    #[tokio::test(flavor = "current_thread")]
    async fn fire_returns_immediately_while_hook_still_runs() {
        let catalog = catalog_from_toml(
            r#"
            [[hooks.pane-exit]]
            action = { kind = "run", command = "sleep 30" }
            "#,
        );
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dispatcher = spawn_hook_dispatcher(catalog, None);
                let started = Instant::now();
                dispatcher.fire(HookEvent::new(PANE_EXIT, []));
                assert!(
                    started.elapsed() < Duration::from_millis(200),
                    "fire must not wait for the hook child",
                );
                // Give the dispatcher a beat to spawn the child, then drop
                // everything: kill_on_drop reaps the sleeping child.
                tokio::time::sleep(Duration::from_millis(10)).await;
            })
            .await;
    }

    /// Poll for `path` to appear (the hook child writes it asynchronously).
    async fn wait_for_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(Instant::now() < deadline, "hook never wrote {path:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // A final beat so the write is complete, not just the file created.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
