mod config;
mod detect;
mod model;
mod offline;
mod record;
mod send_keys;
mod session;
mod shim;
mod wait;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::state::Degradation;
use phux_protocol::wire::info::{SessionSnapshot, TerminalInfo, WindowInfo};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, parse_selector, partial, report_no_server, resolve_targets};

use self::config::configured_agents;
use self::detect::infer_agent_state;
use self::model::{AgentStateReport, PaneEvidence};
use self::record::{run_agent_clear, run_agent_set};

// Shared with the `phux config agents` live projection (phux-r82.10):
// the pipelined per-pane `phux.agent/v1` index and the pane formatter.
pub(crate) use self::model::format_terminal;
pub(crate) use self::record::fetch_agent_index;
pub(crate) use self::session::{
    AgentSessionRecord, PreparedAgentSession, fetch_record_index, persist_record, prepare,
    prepare_for_launch,
};

#[derive(Debug, clap::Subcommand)]
pub(crate) enum AgentAction {
    /// List inferred agent state for every pane.
    #[command(visible_alias = "ls")]
    List {
        /// Emit machine-readable JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Show inferred state for one pane.
    Show {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        target: Option<String>,
        /// Emit machine-readable JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Explain the evidence behind one pane's state.
    ///
    /// With `--file` this runs OFFLINE: it evaluates the compiled detection
    /// manifests against a captured screen and contacts no server at all.
    /// That is the mode for authoring and debugging a manifest, because it
    /// prints the text every region resolved to on that screen — a rule
    /// scoped to a region that comes back empty can never match, and nothing
    /// else makes that visible (the detector fails safe to `idle`, silently).
    ///
    /// The capture is `phux snapshot --json` output or a plain text screen,
    /// one viewport row per line; `-` reads stdin. A capture carries no OSC
    /// title, so pass `--title` to exercise title-scoped rules.
    Explain {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane. Not used in offline (`--file`) mode.
        target: Option<String>,
        /// Emit machine-readable JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Evaluate a captured screen offline instead of querying the
        /// server. `-` reads stdin.
        #[arg(long, value_name = "PATH", conflicts_with = "target")]
        file: Option<PathBuf>,
        /// Agent kind whose manifest to evaluate, or one of its binary
        /// aliases. Required with `--file`: offline there is no foreground
        /// process group to identify the agent from.
        #[arg(long, value_name = "KIND", requires = "file")]
        kind: Option<String>,
        /// OSC 0/2 title to evaluate `title`-scoped rules against. Captures
        /// do not carry one, so it defaults to empty.
        #[arg(long, value_name = "TEXT", requires = "file")]
        title: Option<String>,
        /// How to read `--file`. `auto` picks JSON when the first
        /// non-whitespace byte is `{`.
        #[arg(long, value_parser = ["auto", "json", "text"], requires = "file")]
        format: Option<String>,
    },
    /// Declare a pane's agent identity (writes the phux.agent/v1 L3
    /// record).
    Set {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        target: Option<String>,
        /// Human-facing agent name (required, non-empty).
        #[arg(long)]
        name: String,
        /// Open-vocabulary kind slug, e.g. "claude" or "codex".
        #[arg(long)]
        kind: Option<String>,
        /// Declared lifecycle state.
        #[arg(long, value_parser = ["unknown", "idle", "working", "blocked", "done"])]
        state: Option<String>,
        /// Declared attention priority (defaults derive from state).
        #[arg(long, value_parser = ["none", "low", "normal", "high"])]
        attention: Option<String>,
        /// Free-form association label (fleet/job name).
        #[arg(long)]
        session: Option<String>,
    },
    /// Block until a pane's agent TRANSITIONS into a lifecycle state.
    ///
    /// Satisfied only by an observed transition, never by a level read of
    /// the current state. That distinction is the point of the verb: `idle`
    /// is the detector's fail-safe fallthrough — no shipped detection
    /// manifest asserts it positively — so it is equally true of a finished
    /// agent, a half-painted TUI, a crashed agent, and a pane running
    /// `less`. A gate that fired on that level would return success on a
    /// corpse, instantly, and on any pane with no manifest at all.
    ///
    /// The consequence is deliberate: a pane already resting in a target
    /// state when the wait begins times out (124) rather than succeeding.
    /// `phux agent show` is the level read; this verb reports transitions.
    ///
    /// Subscribes before reading the baseline, so no transition is lost in
    /// between, and re-reads on the `phux wait` cadence to recover an edge a
    /// dropped notification never delivered. A record that goes away
    /// mid-wait ends it as a departure (exit 1), which is not a completion.
    Wait {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        target: Option<String>,
        /// Lifecycle state to wait for; repeat to OR several. Defaults to
        /// `idle`, `blocked`, `done` — the three ways a turn ends.
        /// `unknown` is not spellable: it is departure, not a state.
        #[arg(long, value_name = "STATE", value_parser = ["idle", "working", "blocked", "done"])]
        until: Vec<String>,
        /// Give up after this many seconds and exit 124. Unbounded when
        /// omitted, matching `phux wait` — always pass one in a script.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Emit the machine-readable result document instead of a line.
        #[arg(long)]
        json: bool,
    },
    /// Send keys to a pane, but only if it still hosts the expected agent.
    ///
    /// The agent-addressed sibling of top-level `phux send-keys`, and it
    /// differs from it in exactly one way: it re-reads the pane's
    /// phux.agent/v1 record immediately before writing and refuses if the
    /// occupant changed. `phux send-keys` addresses a pane and deliberately
    /// checks no identity; use that one when a pane is what you mean.
    ///
    /// Every key is validated before any byte is written, so a typo in the
    /// third key cannot leave the first two delivered.
    SendKeys {
        /// Target selector (resolves to one pane).
        target: String,
        /// Key specs: named keys (`Enter`, `C-c`, `M-x`, `Up`) or literal
        /// text. A literal run immediately before `Enter` is sent as one
        /// submission-safe paste.
        #[arg(required = true)]
        keys: Vec<String>,
        /// Require the pane's declared agent name to be this one.
        #[arg(long, value_name = "NAME")]
        expect_agent: Option<String>,
        /// Require the pane's declared agent kind slug to be this one.
        #[arg(long, value_name = "KIND")]
        expect_kind: Option<String>,
        /// Emit machine-readable JSON instead of staying quiet on success.
        #[arg(long)]
        json: bool,
    },
    /// Clear a pane's declared agent identity (deletes phux.agent/v1).
    Clear {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        target: Option<String>,
    },
    /// Make plain `claude` launch inside phux and publish lifecycle state.
    InstallClaude {
        /// Shell rc file to activate (auto-detected from SHELL).
        #[arg(long, value_parser = ["zsh", "bash", "fish"])]
        shell: Option<String>,
        /// Absolute path to the real Claude executable (auto-detected from PATH).
        #[arg(long, value_name = "PATH")]
        real: Option<PathBuf>,
    },
    /// Remove the claude-in-phux shim and shell activation.
    UninstallClaude,
}

pub(crate) fn run_agent(action: &AgentAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        AgentAction::List { json } => run_agent_list(*json, socket),
        // `--file` is the offline manifest debugger: no socket, no runtime,
        // no server. Routed before the live path so a capture can be
        // explained on a machine with no phux running at all.
        AgentAction::Explain {
            json,
            file: Some(path),
            kind,
            title,
            format,
            ..
        } => offline::run(
            path,
            kind.as_deref(),
            title.as_deref(),
            format.as_deref(),
            *json,
        ),
        AgentAction::Show { .. } | AgentAction::Explain { .. } => run_agent_one(action, socket),
        AgentAction::Set {
            target,
            name,
            kind,
            state,
            attention,
            session,
        } => run_agent_set(
            target.as_deref(),
            name,
            kind.as_deref(),
            state.as_deref(),
            attention.as_deref(),
            session.as_deref(),
            socket,
        ),
        AgentAction::Clear { target } => run_agent_clear(target.as_deref(), socket),
        AgentAction::Wait {
            target,
            until,
            timeout,
            json,
        } => wait::run_agent_wait(target.as_deref(), until, *timeout, *json, socket),
        AgentAction::SendKeys {
            target,
            keys,
            expect_agent,
            expect_kind,
            json,
        } => send_keys::run_agent_send_keys(
            target,
            keys,
            expect_agent.as_deref(),
            expect_kind.as_deref(),
            *json,
            socket,
        ),
        AgentAction::InstallClaude { shell, real } => {
            shim::run_install_claude(shell.as_deref(), real.as_deref())
        }
        AgentAction::UninstallClaude => shim::run_uninstall_claude(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentView {
    Show,
    Explain,
}

fn run_agent_list(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (snapshot, degradation) = match fetch_snapshot(&socket_path, "agent list").await {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        let plugins = configured_agents();
        let states = classify_snapshot(&socket_path, &snapshot, &plugins).await;
        // An enumeration, like `phux ls`: every row printed is true, the list
        // is just short. Warn and succeed rather than fail the whole roster
        // over one unreachable satellite.
        partial::warn_partial_view("agent list", &degradation);
        print_agent_states(&states, json, AgentView::Show)
    })
}

fn run_agent_one(action: &AgentAction, socket: Option<PathBuf>) -> ExitCode {
    let (target, json, view) = match action {
        AgentAction::Show { target, json } => (target.as_deref(), *json, AgentView::Show),
        AgentAction::Explain { target, json, .. } => (target.as_deref(), *json, AgentView::Explain),
        AgentAction::List { .. }
        | AgentAction::Set { .. }
        | AgentAction::Clear { .. }
        | AgentAction::Wait { .. }
        | AgentAction::SendKeys { .. }
        | AgentAction::InstallClaude { .. }
        | AgentAction::UninstallClaude => return ExitCode::FAILURE,
    };
    let selector = match parse_selector(target) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (snapshot, degradation) = match fetch_snapshot(&socket_path, "agent").await {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        let candidates = resolve_targets(&socket_path, &selector, &snapshot).await;
        // `show` / `explain` address one Terminal, and `panes` is the list a
        // hub merges — so both misses below are the ambiguous kind whenever
        // the fleet view is partial.
        let Some(target_id) =
            crate::selector::pick_target_pane(&candidates, &snapshot.focused_pane)
        else {
            return partial::report_target_miss(target, &degradation);
        };
        let plugins = configured_agents();
        let states = classify_snapshot(&socket_path, &snapshot, &plugins).await;
        let Some(state) = states
            .into_iter()
            .find(|state| state.terminal == format_terminal(&target_id))
        else {
            return partial::report_target_miss(target, &degradation);
        };
        partial::warn_partial_view("agent", &degradation);
        print_agent_states(&[state], json, view)
    })
}

/// One `GET_STATE`, plus what it could not see.
///
/// Returned as a pair rather than a bare snapshot because the two `agent`
/// readers disagree about what to do with the second half: `list` enumerates
/// (warn, exit 0), `show`/`explain` resolve a Terminal (a miss under
/// degradation is unresolved, not absent).
async fn fetch_snapshot(
    socket_path: &Path,
    verb: &str,
) -> Result<(SessionSnapshot, Degradation), ExitCode> {
    phux_client::state::get_state(socket_path)
        .await
        .map(phux_client::state::StateView::into_parts)
        .map_err(|err| report_no_server(&err, socket_path, verb))
}

async fn classify_snapshot(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
    plugins: &[model::PluginAgent],
) -> Vec<AgentStateReport> {
    // ADR-0040: structured `phux.agent/v1` records outrank every heuristic
    // source, so fetch them up front (one pipelined connection).
    let records = fetch_agent_index(socket_path, snapshot).await;
    let mut states = Vec::with_capacity(snapshot.panes.len());
    for pane in &snapshot.panes {
        let mut evidence = pane_evidence(socket_path, snapshot, pane).await;
        evidence.record = records.get(&pane.id).cloned();
        states.push(infer_agent_state(&evidence, plugins));
    }
    states.sort_by(|a, b| a.terminal.cmp(&b.terminal));
    states
}

async fn pane_evidence(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
    pane: &TerminalInfo,
) -> PaneEvidence {
    let screen =
        phux_client::snapshot::get_screen_scrollback(socket_path, pane.id.clone(), None, true)
            .await
            .ok();
    let window = snapshot.windows.iter().find(|w| w.id == pane.window_id);
    let session = window.and_then(|w| session_for_window(snapshot, w));
    PaneEvidence {
        terminal: format_terminal(&pane.id),
        session: session.map_or_else(|| "unknown".to_owned(), |s| s.name.clone()),
        window: window_label(window),
        title: pane.title.clone(),
        cwd: pane.cwd.clone(),
        record: None,
        lines: screen.as_ref().map_or_else(Vec::new, |s| s.lines.clone()),
        semantic_input: screen
            .as_ref()
            .and_then(|s| s.cells.as_ref())
            .is_some_and(|cells| {
                cells
                    .iter()
                    .any(|cell| cell.semantic == Some(phux_core::screen::SemanticContent::Input))
            }),
    }
}

fn session_for_window<'a>(
    snapshot: &'a SessionSnapshot,
    window: &WindowInfo,
) -> Option<&'a phux_protocol::wire::info::SessionInfo> {
    snapshot
        .sessions
        .iter()
        .find(|session| session.id == window.session_id)
}

fn window_label(window: Option<&WindowInfo>) -> String {
    window.map_or_else(|| "unknown".to_owned(), |w| format!("window-{}", w.index))
}

fn print_agent_states(states: &[AgentStateReport], json: bool, view: AgentView) -> ExitCode {
    if json {
        let value = serde_json::json!({
            "schema_version": 1,
            "agents": states,
        });
        return match serde_json::to_string_pretty(&value) {
            Ok(rendered) => {
                outln!("{rendered}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("phux: could not render agent JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    for state in states {
        outln!(
            "{}\t{}\t{}\t{:.2}\t{}",
            state.terminal,
            state.agent.id,
            state.state,
            state.confidence,
            state.explanation
        );
        if view == AgentView::Explain {
            for source in &state.sources {
                outln!(
                    "  - {} {:.2}: {} ({})",
                    source.kind,
                    source.confidence,
                    source.signal,
                    source.observed
                );
            }
        }
    }
    ExitCode::SUCCESS
}
