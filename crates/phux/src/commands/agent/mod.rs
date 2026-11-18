mod action;
mod action_flags;
mod answer;
mod hook_payload;
mod hook_transcript;
mod list;
mod native_host;
mod notify;
mod offline;
mod prompt;
mod questions;
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
pub(crate) mod tree;
mod wait;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::agent::detect::{AgentStateReport, agents_document};
use phux_client::agent::reads::{self, ReadError};
use phux_client::state::Degradation;
use phux_config::socket::default_socket_path;
use phux_protocol::wire::info::SessionSnapshot;

use crate::commands::{cli_runtime, json_err, parse_selector, partial};

use self::record::{run_agent_clear, run_agent_set};

pub(crate) use self::action::AgentAction;
pub(crate) use self::notify::run_notify;
pub(crate) use self::record::fetch_agent_index;
pub(crate) use self::session::{PreparedAgentSession, prepare, prepare_for_launch};

#[allow(
    clippy::too_many_lines,
    reason = "one dispatch arm per agent verb; splitting the match hides the verb table"
)]
pub(crate) fn run_agent(action: &AgentAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        AgentAction::Host => native_host::run(),
        AgentAction::Questions { target } => questions::current(target, socket),
        AgentAction::RespondQuestion { request_json } => questions::respond(request_json, socket),
        AgentAction::QuestionReceipt {
            question_json,
            operation_id,
        } => questions::receipt(question_json, operation_id, socket),
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
        AgentAction::List { flags } => list::run(flags, socket),
        // `--file` is the offline manifest debugger: no server needed.
        AgentAction::Explain {
            flags:
                action_flags::ExplainFlags {
                    json,
                    file: Some(path),
                    kind,
                    title,
                    format,
                    ..
                },
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
        AgentAction::Explain {
            flags: action_flags::ExplainFlags { target, json, .. },
        } => run_agent_one(target.as_deref(), *json, AgentView::Explain, socket),
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
        AgentAction::Tree { action } => tree::run_tree(action, socket),
        AgentAction::Surface { action } => tree::run_surface(action, socket),
        AgentAction::Doc { action } => tree::run_doc(action, socket),
        AgentAction::Page { action } => tree::run_page(action, socket),
        AgentAction::App { action } => tree::run_app(action, socket),
        AgentAction::Wait {
            target,
            flags:
                action_flags::WaitFlags {
                    any,
                    until,
                    timeout,
                    json,
                },
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
        AgentAction::InstallClaude {
            flags: action_flags::InstallClaudeFlags { shell, real },
        } => shim::run_install_claude(shell.as_deref(), real.as_deref()),
        AgentAction::UninstallClaude => shim::run_uninstall_claude(),
        AgentAction::InstallShim {
            flags:
                action_flags::InstallShimFlags {
                    agent,
                    all,
                    shell,
                    json,
                },
        } => shim::run_install_shim(agent, *all, shell.as_deref(), *json),
        AgentAction::UninstallShim {
            flags: action_flags::UninstallShimFlags { agent, json },
        } => shim::run_uninstall_shim(agent, *json),
        AgentAction::Usage {
            flags:
                action_flags::UsageFlags {
                    json,
                    publish,
                    refresh,
                },
        } => crate::commands::plan_usage::run_usage(*json, *publish, *refresh, socket),
        AgentAction::Stats { publish } => crate::commands::agent_stats::run_stats(*publish, socket),
        AgentAction::HookPayload => hook_payload::run(),
        AgentAction::HookTranscript => hook_transcript::run(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentView {
    Show,
    Explain,
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
        let mut notices = Vec::new();
        let result = reads::show(&socket_path, &selector, &mut notices).await;
        warn_partial_records(&notices);
        match result {
            Ok(projection) => {
                partial::warn_partial_view("agent", &projection.degradation);
                print_agent_states(&projection.reports, json, view)
            }
            Err(err) => report_read_error(json, &err, target, &socket_path, "agent"),
        }
    })
}

/// Report a level read's failure on the verb's own channels.
fn report_read_error(
    json: bool,
    err: &ReadError,
    target: Option<&str>,
    socket_path: &Path,
    verb: &str,
) -> ExitCode {
    match err {
        ReadError::Transport(err) => json_err::report_no_server(json, err, socket_path, verb),
        ReadError::Resolve(err) => crate::commands::report_agent_resolve_error(json, err, false),
        ReadError::Miss(degradation) => partial::report_target_miss_for(json, target, degradation),
    }
}

/// Warn about each record read the projection could only do in part.
fn warn_partial_records(notices: &[String]) {
    for message in notices {
        warn!("partial results — {message}");
    }
}

/// One `GET_STATE`, plus what it could not see. The caller reports a
/// failure on its own channel, so a `--json` verb never gets prose too.
async fn fetch_snapshot(
    socket_path: &Path,
    verb: &str,
) -> Result<(SessionSnapshot, Degradation), phux_client::attach::AttachError> {
    phux_client::state::get_state(socket_path)
        .await
        .map(phux_client::state::StateView::into_parts)
        .inspect_err(|err| tracing::debug!(error = %err, verb, "session state read failed"))
}

fn print_agent_states(states: &[AgentStateReport], json: bool, view: AgentView) -> ExitCode {
    if json {
        return match serde_json::to_string_pretty(&agents_document(states)) {
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

/// Run `phux agent` as parsed.
pub(crate) fn run(
    args: crate::commands::AgentArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let crate::commands::AgentArgs { action } = args;
    run_agent(&action, socket)
}

#[cfg(test)]
mod client_contract;
