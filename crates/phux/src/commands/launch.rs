use std::path::PathBuf;
use std::process::ExitCode;

use phux_config::integration::LaunchWorkingDirectory;
use phux_config::loader as config_loader;
use phux_plugin::{LaunchError, ResolvedLaunch};
use phux_protocol::ids::GroupId;
use phux_protocol::wire::frame::{FrameKind, SpawnResult};
use phux_server::runtime::default_socket_path;

use crate::commands::SpawnSplit;
use crate::commands::agent::{PreparedAgentSession, prepare_for_launch};
use crate::commands::spawn::{dispatch_spawn, dispatch_spawn_placed, report_spawn_error};

/// `phux launch` (phux-ark7, ADR-0042) — resolve a named agent integration
/// template from an enabled plugin and spawn a pane running its `[launch]`
/// command.
///
/// The launch command typically routes the agent through
/// `phux-agent-wrap.sh`, so a launched pane self-declares its
/// `phux.agent/v1` identity (ADR-0040): the server injects
/// `PHUX_TERMINAL_ID` into the spawned pane, the wrapper reads it to target
/// its own pane, and writes name + kind at launch (clearing on exit). No
/// alias, no per-shell config.
///
/// `--print` resolves and prints the argv without spawning (a server-free
/// dry run); `--list` enumerates launchable integrations.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_launch(
    integration: Option<String>,
    list: bool,
    print: bool,
    json: bool,
    target: Option<String>,
    split: SpawnSplit,
    ratio: f32,
    cwd: Option<PathBuf>,
    socket: Option<PathBuf>,
    extra: &[String],
) -> ExitCode {
    let config_path = config_loader::config_path();
    if list {
        return run_list(&config_path, json);
    }
    let Some(integration) = integration else {
        eprintln!("phux: launch requires an INTEGRATION name (or --list)");
        return ExitCode::FAILURE;
    };
    let workspace_cwd = match cwd {
        Some(dir) => dir,
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(err) => {
                eprintln!("phux: launch could not read the current directory: {err}");
                return ExitCode::FAILURE;
            }
        },
    };
    let resolved =
        match phux_plugin::resolve_launch(&config_path, &integration, extra, &workspace_cwd) {
            Ok(resolved) => resolved,
            Err(err) => return report_launch_error(&err),
        };
    let prepared = match prepare_for_launch(&resolved) {
        Ok(prepared) => prepared,
        Err(err) => {
            eprintln!("phux: could not prepare agent session: {err}");
            return ExitCode::FAILURE;
        }
    };
    let argv = prepared
        .as_ref()
        .map_or(&resolved.argv, |session| &session.argv);
    if print {
        return print_resolved(&resolved, argv, json);
    }
    spawn_resolved(
        &resolved,
        prepared.as_ref(),
        socket,
        json,
        target,
        split,
        ratio,
    )
}

fn spawn_resolved(
    resolved: &ResolvedLaunch,
    prepared: Option<&PreparedAgentSession>,
    socket: Option<PathBuf>,
    json: bool,
    target: Option<String>,
    split: SpawnSplit,
    ratio: f32,
) -> ExitCode {
    let argv = prepared.map_or(&resolved.argv, |session| &session.argv);
    let env = prepared.map(|session| {
        session
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    });
    let agent_session = match prepared.map(|session| session.record.encode()).transpose() {
        Ok(record) => record,
        Err(err) => {
            eprintln!("phux: could not encode agent session provenance: {err}");
            return ExitCode::FAILURE;
        }
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let request_id = 1u32;
    let frame = FrameKind::SpawnTerminal {
        request_id,
        // v0.1 servers expose the single default group (SPEC §3.1).
        group: GroupId::new(1),
        command: Some(argv.clone()),
        cwd: Some(resolved.cwd.display().to_string()),
        env,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session,
    };
    let result = match target {
        Some(target) => dispatch_spawn_placed(
            &socket_path,
            frame,
            request_id,
            "launch",
            &target,
            split,
            ratio,
            prepared.map(|session| &session.record),
            json,
        ),
        None => dispatch_spawn(
            &socket_path,
            &frame,
            "launch",
            prepared.map(|session| &session.record),
            json,
        ),
    };
    match result {
        Ok(SpawnResult::Ok(terminal_id)) => print_launched(resolved, argv, &terminal_id, json),
        Ok(SpawnResult::Err(err)) => {
            report_spawn_error(&err);
            ExitCode::FAILURE
        }
        // `SpawnResult` is `#[non_exhaustive]`: a kind with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        Ok(_) => {
            eprintln!(
                "phux: {}",
                phux_client::explain::unexpected_reply("SPAWN_TERMINAL")
            );
            ExitCode::FAILURE
        }
        Err(code) => code,
    }
}

fn run_list(config_path: &std::path::Path, json: bool) -> ExitCode {
    let integrations = match phux_plugin::list_launchable(config_path) {
        Ok(integrations) => integrations,
        Err(err) => return report_launch_error(&err),
    };
    if json {
        let items: Vec<_> = integrations
            .iter()
            .map(|item| {
                serde_json::json!({
                    "integration": item.integration_id,
                    "plugin": item.plugin_id,
                    "display_name": item.display_name,
                    "kind": item.kind,
                })
            })
            .collect();
        let payload = serde_json::json!({ "schema_version": 1, "integrations": items });
        return match serde_json::to_string_pretty(&payload) {
            Ok(rendered) => {
                outln!("{rendered}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("phux: could not render launch list JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    if integrations.is_empty() {
        eprintln!(
            "phux: no launchable integrations in any enabled plugin \
             (install one, e.g. the agent-tools plugin, then `phux plugin enable`)"
        );
        return ExitCode::SUCCESS;
    }
    for item in &integrations {
        let name = item.display_name.as_deref().unwrap_or(&item.integration_id);
        outln!("{}\t{}\t{}", item.integration_id, name, item.plugin_id);
    }
    ExitCode::SUCCESS
}

fn print_resolved(resolved: &ResolvedLaunch, argv: &[String], json: bool) -> ExitCode {
    if json {
        let payload = serde_json::json!({
            "schema_version": 1,
            "integration": resolved.integration_id,
            "plugin": resolved.plugin_id,
            "cwd": resolved.cwd.display().to_string(),
            "working_directory": working_directory_slug(resolved.working_directory),
            "argv": argv,
        });
        return match serde_json::to_string_pretty(&payload) {
            Ok(rendered) => {
                outln!("{rendered}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("phux: could not render launch JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    outln!("{} ({})", resolved.integration_id, resolved.plugin_id);
    outln!("  cwd: {}", resolved.cwd.display());
    outln!("  argv: {}", argv.join(" "));
    ExitCode::SUCCESS
}

fn print_launched(
    resolved: &ResolvedLaunch,
    argv: &[String],
    terminal_id: &phux_protocol::ids::TerminalId,
    json: bool,
) -> ExitCode {
    let id = match terminal_id {
        phux_protocol::ids::TerminalId::Local { id }
        | phux_protocol::ids::TerminalId::Satellite { id, .. } => *id,
    };
    if json {
        let payload = serde_json::json!({
            "schema_version": 1,
            "terminal_id": id,
            "integration": resolved.integration_id,
            "plugin": resolved.plugin_id,
            "argv": argv,
        });
        return match serde_json::to_string_pretty(&payload) {
            Ok(rendered) => {
                outln!("{rendered}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("phux: could not render launch result JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    outln!(
        "launched {} in terminal {id} ({})",
        resolved.integration_id,
        resolved.plugin_id
    );
    ExitCode::SUCCESS
}

const fn working_directory_slug(dir: LaunchWorkingDirectory) -> &'static str {
    match dir {
        LaunchWorkingDirectory::Workspace => "workspace",
        LaunchWorkingDirectory::PluginRoot => "plugin-root",
    }
}

fn report_launch_error(err: &LaunchError) -> ExitCode {
    match err {
        LaunchError::NotFound { name, available } if available.is_empty() => {
            eprintln!(
                "phux: no launchable integration named {name:?} in any enabled plugin \
                 (install one, then `phux plugin enable`)"
            );
        }
        LaunchError::NotFound { name, available } => {
            eprintln!(
                "phux: no launchable integration named {name:?}; available: {}",
                available.join(", ")
            );
        }
        LaunchError::NoLaunchCommand { name } => {
            eprintln!("phux: integration {name:?} declares no `[launch]` command");
        }
        other => eprintln!("phux: launch failed: {other}"),
    }
    ExitCode::FAILURE
}
