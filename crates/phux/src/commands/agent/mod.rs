mod action;
mod action_flags;
mod answer;
mod config;
mod detect;
mod hook_payload;
mod hook_transcript;
mod model;
mod native_host;
mod offline;
mod prompt;
mod record;
mod report_state;
// AgentSession resource verbs (ADR-0103); unrelated to `session.rs`, the
// provider-native resume record in L3 metadata.
mod native_control;
mod resource_session;
mod send_keys;
mod session;
pub(crate) mod shim;
mod start;
mod wait;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::state::Degradation;
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, parse_selector, partial, report_no_server, resolve_targets};

use self::config::configured_agents;
use self::detect::infer_agent_state;
use self::model::{AgentStateReport, PaneEvidence};
use self::record::{run_agent_clear, run_agent_set};

pub(crate) use self::action::AgentAction;
pub(crate) use self::record::fetch_agent_index;
pub(crate) use self::session::{PreparedAgentSession, prepare, prepare_for_launch};

#[allow(
    clippy::too_many_lines,
    reason = "one dispatch arm per agent verb; splitting the match hides the verb table"
)]
pub(crate) fn run_agent(action: &AgentAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        AgentAction::Host => native_host::run(),
        AgentAction::NativeCommand {
            target,
            flags:
                action_flags::NativeCommandFlags {
                    activation,
                    operation_id,
                    intent,
                    json,
                },
        } => native_control::run(target, activation, *operation_id, intent, *json, socket),
        AgentAction::List { json } => run_agent_list(*json, socket),
        // `--file` is the offline manifest debugger: no server needed.
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
        AgentAction::Show { target, json } => {
            run_agent_one(target.as_deref(), *json, AgentView::Show, socket)
        }
        AgentAction::Explain { target, json, .. } => {
            run_agent_one(target.as_deref(), *json, AgentView::Explain, socket)
        }
        AgentAction::Set {
            target,
            flags:
                action_flags::SetFlags {
                    name,
                    kind,
                    state,
                    attention,
                    session,
                    json,
                },
        } => run_agent_set(
            target.as_deref(),
            name,
            kind.as_deref(),
            state.as_deref(),
            attention.as_deref(),
            session.as_deref(),
            *json,
            socket,
        ),
        AgentAction::ReportState { target, state } => report_state::run(target, state, socket),
        AgentAction::Answer {
            target,
            flags:
                action_flags::AnswerFlags {
                    id,
                    choice,
                    text,
                    allow_unlisted,
                    json,
                },
        } => answer::run_agent_answer(
            target,
            id,
            *choice,
            text.as_deref(),
            *allow_unlisted,
            *json,
            socket,
        ),
        AgentAction::Start {
            name,
            args,
            flags:
                action_flags::StartFlags {
                    kind,
                    target,
                    integration,
                    timeout,
                    no_wait,
                    force,
                    json,
                },
        } => start::start(
            &start::StartRequest {
                name,
                kind,
                target,
                integration: integration.as_deref(),
                timeout: *timeout,
                no_wait: *no_wait,
                force: *force,
                json: *json,
                args,
            },
            socket,
        ),
        AgentAction::Clear { target, json } => run_agent_clear(target.as_deref(), *json, socket),
        AgentAction::Session { action } => match action {
            resource_session::SessionAction::Open {
                target,
                provider,
                native_id,
                json,
            } => resource_session::run_session_open(
                target,
                provider,
                native_id.as_deref(),
                *json,
                socket,
            ),
            resource_session::SessionAction::Close { target } => {
                resource_session::run_session_close(target, socket)
            }
        },
        AgentAction::Emit {
            target,
            flags:
                action_flags::EmitFlags {
                    event_type,
                    data,
                    json,
                },
        } => resource_session::run_emit(target, event_type, data.as_deref(), *json, socket),
        AgentAction::Log {
            target,
            flags: action_flags::LogFlags { follow, tail, json },
        } => resource_session::run_log(target, *follow, *tail, *json, socket),
        AgentAction::Wait {
            target,
            any,
            until,
            timeout,
            json,
        } => wait::run_agent_wait(target.as_deref(), *any, until, *timeout, *json, socket),
        AgentAction::Prompt {
            target,
            text,
            flags:
                action_flags::PromptFlags {
                    expect_agent,
                    expect_kind,
                    wait,
                    until,
                    timeout,
                    json,
                },
        } => prompt::run_agent_prompt(
            target,
            text,
            expect_agent.as_deref(),
            expect_kind.as_deref(),
            *wait,
            until,
            *timeout,
            *json,
            socket,
        ),
        AgentAction::SendKeys {
            target,
            keys,
            flags:
                action_flags::SendKeysFlags {
                    expect_agent,
                    expect_kind,
                    json,
                },
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
        AgentAction::Usage {
            flags:
                action_flags::UsageFlags {
                    json,
                    publish,
                    refresh,
                },
        } => crate::commands::plan_usage::run_usage(*json, *publish, *refresh, socket),
        AgentAction::HookPayload => hook_payload::run(),
        AgentAction::HookTranscript => hook_transcript::run(),
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
        // An enumeration, like `phux ls`: warn about a partial view and succeed.
        partial::warn_partial_view("agent list", &degradation);
        print_agent_states(&states, json, AgentView::Show)
    })
}

fn run_agent_one(
    target: Option<&str>,
    json: bool,
    view: AgentView,
    socket: Option<PathBuf>,
) -> ExitCode {
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
        // `%name` is singular (ADR-0075 point 3): the agent's pane, or a
        // refusal that names why — never `pick_target_pane` over a set.
        let candidates = match resolve_targets(&socket_path, &selector, &snapshot).await {
            Ok(candidates) => candidates,
            Err(err) => return crate::commands::report_agent_resolve_error(json, &err, false),
        };
        let Some(target_id) =
            phux_client::selector::pick_target_pane(&candidates, &snapshot.focused_resource)
        else {
            return partial::report_target_miss(target, &degradation);
        };
        let plugins = configured_agents();
        let states = classify_snapshot(&socket_path, &snapshot, &plugins).await;
        let Some(state) = states
            .into_iter()
            .find(|state| state.terminal == phux_client::selector::format_terminal_id(&target_id))
        else {
            return partial::report_target_miss(target, &degradation);
        };
        partial::warn_partial_view("agent", &degradation);
        print_agent_states(&[state], json, view)
    })
}

/// One `GET_STATE`, plus what it could not see.
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
    // Structured `phux.agent/v1` records outrank every heuristic (ADR-0040).
    let records = fetch_agent_index(socket_path, snapshot).await;
    // What `%name` would make of each listed record (ADR-0075 point 4).
    let index = phux_client::selector::AgentIndex::complete(records);
    let mut states = Vec::with_capacity(snapshot.resources.len());
    // Terminal-kind resources only: an `AgentSession` is reported under its
    // parent's `agent_session`, never as a row of its own.
    for pane in phux_client::resource::terminals(snapshot) {
        let mut evidence = pane_evidence(socket_path, snapshot, pane).await;
        evidence.record = index.get(&pane.id).cloned();
        let mut state = infer_agent_state(&evidence, plugins);
        state.set_address(phux_client::selector::agent_address(
            &pane.id, snapshot, &index,
        ));
        states.push(state);
    }
    states.sort_by(|a, b| a.terminal.cmp(&b.terminal));
    states
}

async fn pane_evidence(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
    pane: &ResourceInfo,
) -> PaneEvidence {
    let (screen, occupant) = tokio::join!(
        phux_client::snapshot::get_screen_scrollback(socket_path, pane.id.clone(), None, true),
        phux_client::agent_meta::read_pane_occupant_within(
            socket_path,
            &pane.id,
            std::time::Duration::ZERO
        ),
    );
    let screen = screen.ok();
    let session = session_of_pane(snapshot, pane);
    // The pane's live agent session only when it has exactly one (ADR-0103).
    let agent_session = {
        let mut children = phux_client::resource::children_of(snapshot, &pane.id);
        match (children.next(), children.next()) {
            (Some(child), None) => child.agent.as_ref().map(|facet| model::SessionEvidence {
                resource: phux_client::selector::format_terminal_id(&child.id),
                provider: facet.provider.clone(),
                native_id: facet.native_id.clone(),
                state: phux_client::agent_meta::AgentMetaState::from(facet.state.clone()),
            }),
            _ => None,
        }
    };
    PaneEvidence {
        terminal: phux_client::selector::format_terminal_id(&pane.id),
        session: session.map_or_else(|| "unknown".to_owned(), |s| s.name.clone()),
        window: window_label(snapshot, pane, session.is_some()),
        // The live OSC title from `GET_SCREEN`, the one the server's detector
        // evaluates. `pane.title` is the user-set title and is not it.
        title: screen.as_ref().and_then(|s| s.title.clone()),
        cwd: pane.cwd.clone(),
        record: None,
        agent_session,
        lines: screen.as_ref().map_or_else(Vec::new, |s| s.lines.clone()),
        semantic_input: screen
            .as_ref()
            .and_then(|s| s.cells.as_ref())
            .is_some_and(|cells| {
                cells
                    .iter()
                    .any(|cell| cell.semantic == Some(phux_core::screen::SemanticContent::Input))
            }),
        occupant,
    }
}

/// The session `pane` belongs to, through its `session` facet
/// ([`phux_client::resource::session_of`]).
fn session_of_pane<'a>(
    snapshot: &'a SessionSnapshot,
    pane: &ResourceInfo,
) -> Option<&'a phux_protocol::wire::info::SessionInfo> {
    let id = phux_client::resource::session_of(snapshot, &pane.id)?;
    snapshot.sessions.iter().find(|session| session.id == id)
}

/// The `window` field of `phux agent --json`, kept for output stability.
/// A session is one window (ADR-0160), labelled `window-0` as the legacy
/// snapshot labels it, so a pane in a known session needs no window lookup.
/// Only a pane from a server that does not report its `session` facet is
/// labelled by its legacy window's index.
fn window_label(snapshot: &SessionSnapshot, pane: &ResourceInfo, in_session: bool) -> String {
    if pane.session.is_some() {
        return if in_session { "window-0" } else { "unknown" }.to_owned();
    }
    snapshot
        .windows
        .iter()
        .find(|window| window.id == pane.window_id)
        .map_or_else(|| "unknown".to_owned(), |w| format!("window-{}", w.index))
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
                error!("could not render agent JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    if states.is_empty() {
        outln!(
            "No panes are reporting agents. Run `phux launch --list` to see what you can start."
        );
        return ExitCode::SUCCESS;
    }
    for state in states {
        outln!(
            "{}\t{}\t{}\t{}\t{:.2}\t{}",
            state.terminal,
            state.agent.id,
            state.address_column(),
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

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

    use super::{session_of_pane, window_label};

    fn snapshot(pane: &ResourceInfo) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), pane.window_id, pane.id.clone())
            .with_sessions(vec![SessionInfo::new(SessionId::new(1), "work")])
            .with_resources(vec![pane.clone()])
    }

    /// A pane joined to its session by the `session` facet alone (no
    /// legacy window) still reports its session and the documented
    /// `window-0` label.
    #[test]
    fn a_facet_pane_names_its_session_without_a_window() {
        let pane = ResourceInfo::new(ResourceId::local(3), WindowId::new(0), 80, 24)
            .with_session(Some(SessionId::new(1)));
        let snapshot = snapshot(&pane);

        let session = session_of_pane(&snapshot, &pane);

        assert_eq!(session.map(|s| s.name.as_str()), Some("work"));
        assert_eq!(
            window_label(&snapshot, &pane, session.is_some()),
            "window-0"
        );
    }

    /// A satellite pane's facet names a session on the satellite, never one
    /// of these: both fields stay `unknown`.
    #[test]
    fn a_satellite_pane_is_unknown() {
        let pane = ResourceInfo::new(ResourceId::satellite("edge", 3), WindowId::new(0), 80, 24)
            .with_session(Some(SessionId::new(1)));
        let snapshot = snapshot(&pane);

        assert!(session_of_pane(&snapshot, &pane).is_none());
        assert_eq!(window_label(&snapshot, &pane, false), "unknown");
    }

    /// A server that predates the facet: the legacy window names both.
    #[test]
    fn a_pre_facet_pane_falls_back_to_its_window() {
        let pane = ResourceInfo::new(ResourceId::local(3), WindowId::new(2), 80, 24);
        let snapshot = snapshot(&pane).with_windows(vec![
            WindowInfo::new(WindowId::new(2), SessionId::new(1), "w").with_index(1),
        ]);

        let session = session_of_pane(&snapshot, &pane);

        assert_eq!(session.map(|s| s.name.as_str()), Some("work"));
        assert_eq!(window_label(&snapshot, &pane, true), "window-1");
    }
}
