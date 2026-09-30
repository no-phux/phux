//! Server-side event-hook dispatcher (`docs/consumers/tui.md` §9).
//!
//! Config `[[hooks.<name>]]` entries (first match wins; only `run` and the
//! `noop` sentinel act server-side) and enabled plugins' `[[events]]` hooks
//! (all matches fire) share one dispatcher. Hooks run as child processes
//! via [`phux_plugin::run_command_spec`] with the context in `PHUX_*` env
//! vars, including `PHUX_SOCKET` so a hook's `phux` reaches this server.
//!
//! [`HookDispatcher::fire`] is a non-blocking `try_send` onto a bounded
//! queue (a full queue drops the event); the dispatcher runs each command on
//! its own task, at most [`MAX_CONCURRENT_HOOKS`] at once. The `hooks` ↔
//! `state` import cycle is deliberate: it keeps `ClientId` typed.

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

/// Depth of the event queue; a full queue drops the event.
pub const HOOK_EVENT_QUEUE: usize = 64;

/// Per-hook timeout; the child is killed and logged.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// Hook point names (defined in [`phux_config::vocab`]).
pub use phux_config::vocab::{
    AFTER_NEW_PANE, AGENT_STATE_CHANGED, CLIENT_ATTACHED, CLIENT_DETACHED, FOCUS_CHANGED, PANE_EXIT,
};

/// One hook event as the generated reference documents it: the vocab's
/// name and context keys plus this dispatcher's "fires when" prose. A test
/// pins it to the real constructors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEventSpec {
    /// Canonical event name, a member of [`phux_config::vocab::HOOK_EVENTS`].
    pub name: &'static str,
    /// When the server fires the event — one sentence of prose.
    pub doc: &'static str,
    /// The event's context keys, sorted (optional ones included).
    pub context_keys: &'static [&'static str],
}

/// Every hook event, in [`vocab::HOOK_EVENTS`] order, for the generated
/// `docs/reference/hooks.md`.
#[must_use]
pub fn hook_event_specs() -> Vec<HookEventSpec> {
    vocab::HOOK_EVENTS
        .iter()
        .map(|&name| HookEventSpec {
            name,
            doc: event_doc(name),
            context_keys: vocab::hook_context_keys(name).unwrap_or(&[]),
        })
        .collect()
}

/// The "fires when" sentence for an event name (a test enforces coverage).
fn event_doc(name: &str) -> &'static str {
    match name {
        AFTER_NEW_PANE => {
            "A pane's actor spawned: fires right after pane creation, \
             before the inner process has produced output."
        }
        PANE_EXIT => {
            "A pane's inner process exited. `exit-code` is present only \
             when the OS reported a code (absent for a signal-killed \
             child)."
        }
        FOCUS_CHANGED => "A client's focus landed on a pane.",
        CLIENT_ATTACHED => "A client's attach completed.",
        CLIENT_DETACHED => {
            "An attached client detached for any reason (explicit detach \
             or transport drop). `session` is absent if the session was \
             reaped before the detach ran."
        }
        AGENT_STATE_CHANGED => {
            "The detector's published agent state for a pane actually \
             changed (ADR-0046). `from` is absent on a first sighting; \
             `agent-name` is absent for an anonymous agent; a withdrawn \
             record arrives as `to = \"unknown\"`."
        }
        other => unreachable!("no doc prose for hook event `{other}`"),
    }
}

/// The `to` value when the detector withdraws a record (the L3 `unknown`).
pub const AGENT_STATE_UNKNOWN: &str = "unknown";

/// One fired event: a §9 name plus context. Keys are kebab-case and reach the
/// child as `PHUX_<KEY>`.
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
        terminal_id: &phux_protocol::ids::ResourceId,
        session: Option<&str>,
    ) -> Self {
        let mut context = terminal_context(terminal_id);
        if let Some(session) = session {
            context.push(("session".to_owned(), session.to_owned()));
        }
        Self::new(AFTER_NEW_PANE, context)
    }

    /// [`PANE_EXIT`]: a pane's process exited; `exit-code` only when known.
    #[must_use]
    pub fn pane_exit(
        terminal_id: &phux_protocol::ids::ResourceId,
        exit_status: Option<i32>,
    ) -> Self {
        let mut context = terminal_context(terminal_id);
        if let Some(code) = exit_status {
            context.push(("exit-code".to_owned(), code.to_string()));
        }
        Self::new(PANE_EXIT, context)
    }

    /// [`FOCUS_CHANGED`]: a client's focus landed on a pane.
    #[must_use]
    pub fn focus_changed(
        terminal_id: &phux_protocol::ids::ResourceId,
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

    /// [`CLIENT_DETACHED`]: a client detached; `session` may be absent if
    /// already reaped.
    #[must_use]
    pub fn client_detached(client_id: crate::state::ClientId, session: Option<&str>) -> Self {
        let mut context = vec![("client-id".to_owned(), client_id.0.to_string())];
        if let Some(session) = session {
            context.push(("session".to_owned(), session.to_owned()));
        }
        Self::new(CLIENT_DETACHED, context)
    }

    /// [`AGENT_STATE_CHANGED`]: the detector's published state changed
    /// (ADR-0046); the notification seam for host tools. `from` is absent
    /// on a first sighting.
    #[must_use]
    pub fn agent_state_changed(
        terminal_id: &phux_protocol::ids::ResourceId,
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
fn terminal_context(terminal_id: &phux_protocol::ids::ResourceId) -> Vec<(String, String)> {
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

/// Config hook entries plus enabled plugins' event hooks.
#[derive(Debug, Clone, Default)]
pub struct HookCatalog {
    /// `[[hooks.<name>]]` entries keyed by hook name.
    pub config_hooks: BTreeMap<String, Vec<HookEntry>>,
    /// Event hooks resolved from enabled plugin manifests.
    pub plugin_events: Vec<PluginEventHook>,
}

impl HookCatalog {
    /// Nothing to dispatch (the runtime then spawns no dispatcher).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.config_hooks.is_empty() && self.plugin_events.is_empty()
    }

    /// Build a catalog from a loaded config. Plugin manifest paths resolve
    /// against the config's directory; disabled plugins and other-platform
    /// events are skipped; unloadable manifests are logged. Config entries
    /// that can never act are warned about once but kept.
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
            let manifest_path = plugin::resolve_manifest_path(&entry.manifest, config_path);
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

/// A config hook entry that can never act, with a log label.
#[derive(Debug, PartialEq, Eq)]
struct HookConfigProblem {
    label: String,
    detail: String,
}

/// Every config hook entry `phux config check` would flag, one record each,
/// using the same vocabulary (unknown event, unknown `when` keys, a
/// never-executable action).
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
            let details = hook_entry_problems(event, context_keys, entry);
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

/// Every reason one entry of a known `event` can never act.
fn hook_entry_problems(event: &str, context_keys: &[&str], entry: &HookEntry) -> Vec<String> {
    let mut details: Vec<String> = vocab::unknown_hook_when_keys(context_keys, &entry.when)
        .map(|key| {
            format!(
                "when key `{key}` can never match (`{event}` context keys: {})",
                context_keys.join(", "),
            )
        })
        .collect();
    if let Some(name) = vocab::dead_hook_action(&entry.action) {
        details.push(format!(
            "action `{name}` never executes server-side (only `run` with a usable \
             `command` does); a match still consumes the event"
        ));
    }
    details
}

/// Whether the current OS is allowed (`None` means every platform).
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
    /// Queue `event` without blocking; drops (logged) when full or gone.
    /// Do not call with the state lock held.
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

    /// A dispatcher over a raw queue (tests).
    #[cfg(test)]
    pub(crate) const fn from_sender(tx: mpsc::Sender<HookEvent>) -> Self {
        Self { tx }
    }
}

/// Fire `event` through the state's dispatcher, if any. Takes the state lock
/// briefly, so never call it inside `with`/`with_mut`.
pub(crate) fn fire_hook(state: &crate::state::SharedState, event: HookEvent) {
    let Some(dispatcher) = state.with(|s| s.hook_dispatcher().cloned()) else {
        return;
    };
    dispatcher.fire(event);
}

/// Spawn the dispatcher on the current `LocalSet` and return its handle.
///
/// Each matched command runs on its own task behind the concurrency cap,
/// with [`HOOK_TIMEOUT`] and `kill_on_drop`; `server_socket` becomes
/// `PHUX_SOCKET`. Exits when every handle is dropped.
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
                    // The semaphore is never closed.
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

/// Every command `event` fires: the first matching config entry plus every
/// matching plugin hook.
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
            // The first match consumes the event even if not executable.
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

/// The hook child's env: `PHUX_EVENT`, `PHUX_<KEY>` per context key, and
/// `PHUX_SOCKET` when known.
fn event_env(event: &HookEvent, server_socket: Option<&Path>) -> Vec<(String, String)> {
    let mut env = vec![("PHUX_EVENT".to_owned(), event.name.clone())];
    for (key, value) in &event.context {
        env.push((context_env_var(key), value.clone()));
    }
    if let Some(path) = server_socket {
        env.push(("PHUX_SOCKET".to_owned(), path.display().to_string()));
    }
    env
}

/// The env var a context key rides as (`exit-code` → `PHUX_EXIT_CODE`),
/// shared with the generated reference.
#[must_use]
pub fn context_env_var(key: &str) -> String {
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

/// Whether all `when` clauses hold: `"*"` matches, `<key>-startswith`
/// prefix-matches, anything else is an exact match (TOML scalars compared
/// via their rendering).
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

/// Render a TOML scalar like context strings (`0` → `"0"`).
fn toml_scalar_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// A config action's argv: only `run` executes (a string command runs via
/// `/bin/sh -c`, an array as argv); `noop` and client-side kinds are `None`.
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
    fn when_clauses_match_context() {
        type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], bool);
        let cases: &[Case<'_>] = &[
            ("", &[], true),
            ("", &[("exit-code", "1")], true),
            ("exit-code = 0", &[("exit-code", "0")], true),
            ("exit-code = 0", &[("exit-code", "1")], false),
            ("exit-code = 0", &[], false),
            ("session = \"work\"", &[("session", "work")], true),
            ("session = \"work\"", &[("session", "home")], false),
            // `"*"` fires even without the key (a signal-killed child).
            ("exit-code = \"*\"", &[("exit-code", "137")], true),
            ("exit-code = \"*\"", &[], true),
            (
                "cwd-startswith = \"/Users/x/work\"",
                &[("cwd", "/Users/x/work/repo")],
                true,
            ),
            (
                "cwd-startswith = \"/Users/x/work\"",
                &[("cwd", "/tmp")],
                false,
            ),
            ("cwd-startswith = \"/Users/x/work\"", &[], false),
            (
                "exit-code = 0\nsession = \"work\"",
                &[("exit-code", "0"), ("session", "work")],
                true,
            ),
            (
                "exit-code = 0\nsession = \"work\"",
                &[("exit-code", "0"), ("session", "home")],
                false,
            ),
        ];
        for (clauses, context, want) in cases {
            assert_eq!(
                when_matches(&when(clauses), &ctx(context)),
                *want,
                "{clauses:?} vs {context:?}"
            );
        }
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

    /// `vocab::hook_action_is_executable` agrees with [`action_argv`] on every
    /// shape.
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

    /// `vocab::hook_context_keys` matches the constructors exactly.
    #[test]
    fn vocab_context_keys_match_the_event_constructors() {
        let terminal = phux_protocol::ids::ResourceId::local(7);
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

    /// Each offending entry yields one problem record; a clean config none.
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
        assert_eq!(context_env_var("exit-code"), "PHUX_EXIT_CODE");
        assert_eq!(context_env_var("terminal-id"), "PHUX_TERMINAL_ID");
        assert_eq!(context_env_var("session"), "PHUX_SESSION");
    }

    /// The spec table and the constructors describe the same events and
    /// keys, with prose for each.
    #[test]
    fn spec_table_roundtrips_through_the_constructors() {
        let specs = hook_event_specs();
        assert_eq!(
            specs.iter().map(|spec| spec.name).collect::<Vec<_>>(),
            vocab::HOOK_EVENTS,
            "one spec per vocab event, in vocab order",
        );
        for spec in &specs {
            assert!(!spec.doc.is_empty(), "no doc prose for `{}`", spec.name);
            assert_eq!(
                Some(spec.context_keys),
                vocab::hook_context_keys(spec.name),
                "spec keys for `{}` must come from the vocab",
                spec.name,
            );
        }

        let terminal = phux_protocol::ids::ResourceId::local(7);
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
            specs.len(),
            "a constructor is missing from this roundtrip",
        );
        for event in events {
            let spec = specs
                .iter()
                .find(|spec| spec.name == event.name)
                .unwrap_or_else(|| panic!("constructor built unspecced event `{}`", event.name));
            let got: Vec<&str> = event.context.keys().map(String::as_str).collect();
            assert_eq!(
                got, spec.context_keys,
                "context keys drifted for `{}`",
                event.name,
            );
        }
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
        // The noop entry consumes the event, so the catch-all does not fire.
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
        // No socket configured: no PHUX_SOCKET.
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

    /// A config `run` hook executes with the event env and `PHUX_SOCKET`.
    #[tokio::test(flavor = "current_thread")]
    async fn dispatcher_executes_config_hook_with_env_injection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("marker");
        let socket = dir.path().join("phux.sock");
        let command = format!(
            "printf '%s %s %s' \"$PHUX_EVENT\" \"$PHUX_TERMINAL_ID\" \"$PHUX_SOCKET\" > {}",
            marker.display()
        );
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

    /// A plugin hook runs in the plugin root with its identity env.
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

    /// A slow hook does not block `fire`.
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
                // Let it spawn, then drop everything (kill_on_drop reaps).
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
