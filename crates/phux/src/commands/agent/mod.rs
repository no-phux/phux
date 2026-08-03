mod config;
mod detect;
mod model;
mod record;
mod session;
mod shim;

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
    Explain {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        target: Option<String>,
        /// Emit machine-readable JSON instead of the table.
        #[arg(long)]
        json: bool,
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
        AgentAction::Show { target: _, json: _ } | AgentAction::Explain { target: _, json: _ } => {
            run_agent_one(action, socket)
        }
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
        AgentAction::Explain { target, json } => (target.as_deref(), *json, AgentView::Explain),
        AgentAction::List { .. }
        | AgentAction::Set { .. }
        | AgentAction::Clear { .. }
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
