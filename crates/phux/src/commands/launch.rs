use std::path::PathBuf;
use std::process::ExitCode;

use phux_config::integration::LaunchWorkingDirectory;
use phux_config::loader as config_loader;
use phux_plugin::{LaunchError, LaunchMode, ResolvedLaunch};
use phux_protocol::ids::GroupId;
use phux_protocol::wire::frame::{FrameKind, SpawnResult};
use phux_server::runtime::default_socket_path;

use crate::commands::SpawnSplit;
use crate::commands::agent::{PreparedAgentSession, prepare_for_launch};
use crate::commands::spawn::{dispatch_spawn, dispatch_spawn_placed, report_spawn_error};

mod native;

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
    native: bool,
    session: Option<&str>,
    json: bool,
    target: Option<String>,
    split: SpawnSplit,
    ratio: f32,
    projection: Option<&str>,
    cwd: Option<PathBuf>,
    socket: Option<PathBuf>,
    extra: &[String],
) -> ExitCode {
    let config_path = config_loader::config_path();
    let mode = if native {
        LaunchMode::Native
    } else {
        LaunchMode::Terminal
    };
    if list {
        return run_list(&config_path, mode, json);
    }
    let Some(integration) = integration else {
        error!("launch requires an INTEGRATION name (or --list)");
        return ExitCode::FAILURE;
    };
    let workspace_cwd = match cwd {
        Some(dir) => dir,
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(err) => {
                error!("launch could not read the current directory: {err}");
                return ExitCode::FAILURE;
            }
        },
    };
    let resolved = match phux_plugin::resolve_launch(
        &config_path,
        &integration,
        extra,
        &workspace_cwd,
        mode,
    ) {
        Ok(resolved) => resolved,
        Err(err) => return report_launch_error(&err),
    };
    if native {
        if print {
            return print_resolved(&resolved, &resolved.argv, json);
        }
        return native::spawn(&resolved, session, socket.as_deref(), json);
    }
    let prepared = match prepare_for_launch(&resolved) {
        Ok(prepared) => prepared,
        Err(err) => {
            error!("could not prepare agent session: {err}");
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
        projection,
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_resolved(
    resolved: &ResolvedLaunch,
    prepared: Option<&PreparedAgentSession>,
    socket: Option<PathBuf>,
    json: bool,
    target: Option<String>,
    split: SpawnSplit,
    ratio: f32,
    projection: Option<&str>,
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
            error!("could not encode agent session provenance: {err}");
            return ExitCode::FAILURE;
        }
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let request_id = 1u32;
    let frame = FrameKind::SpawnResource {
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
        initial_size: None,
        resource: None,
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
            projection,
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
        Ok(SpawnResult::Err(err)) => report_spawn_error(json, &err),
        // `SpawnResult` is `#[non_exhaustive]`: a kind with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        Ok(_) => {
            error!(
                "{}",
                phux_client::explain::unexpected_reply("SPAWN_RESOURCE")
            );
            ExitCode::FAILURE
        }
        Err(code) => code,
    }
}

fn run_list(config_path: &std::path::Path, mode: LaunchMode, json: bool) -> ExitCode {
    let integrations = match phux_plugin::list_launchable(config_path, mode) {
        Ok(integrations) => integrations,
        Err(err) => return report_launch_error(&err),
    };
    if json {
        let items: Vec<_> = integrations
            .iter()
            .map(|item| {
                let native_detail = native::availability(item);
                serde_json::json!({
                    "integration": item.integration_id,
                    "plugin": item.plugin_id,
                    "display_name": item.display_name,
                    "kind": item.kind,
                    // The detection slug from `[agent_identity]` — the value
                    // `phux agent start --kind` resolves against — as
                    // distinct from the category `kind` above. Additive.
                    "agent_kind": item.agent_identity.as_ref()
                        .and_then(|identity| identity.kind.clone()),
                    "native": item.native.is_some(),
                    "native_available": item.native.is_some() && native_detail.is_none(),
                    "native_detail": native_detail,
                })
            })
            .collect();
        let payload = serde_json::json!({ "schema_version": 1, "integrations": items });
        return crate::output::json(&payload);
    }
    if integrations.is_empty() {
        outln!(
            "No agent integrations available. Install a plugin with `phux plugin install SOURCE`."
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
            "native": resolved.native.as_ref().map(|native| serde_json::json!({
                "backend": native.backend.as_str(), "provider": native.provider,
            })),
        });
        return crate::output::json(&payload);
    }
    outln!("{} ({})", resolved.integration_id, resolved.plugin_id);
    outln!("  cwd: {}", resolved.cwd.display());
    outln!("  argv: {}", argv.join(" "));
    ExitCode::SUCCESS
}

fn print_launched(
    resolved: &ResolvedLaunch,
    argv: &[String],
    terminal_id: &phux_protocol::ids::ResourceId,
    json: bool,
) -> ExitCode {
    let id = match terminal_id {
        phux_protocol::ids::ResourceId::Local { id }
        | phux_protocol::ids::ResourceId::Satellite { id, .. } => *id,
    };
    if json {
        let payload = serde_json::json!({
            "schema_version": 1,
            "terminal_id": id,
            "integration": resolved.integration_id,
            "plugin": resolved.plugin_id,
            "argv": argv,
        });
        return crate::output::json(&payload);
    }
    outln!(
        "Launched {} in pane @{id}. Next: `phux agent show @{id}`.",
        resolved.integration_id
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
            error!(
                "no launchable integration named {name:?} in any enabled plugin \
                 (install one, then `phux plugin enable`)"
            );
        }
        LaunchError::NotFound { name, available } => {
            error!(
                "no launchable integration named {name:?}; available: {}",
                available.join(", ")
            );
        }
        LaunchError::NoLaunchCommand { name } => {
            error!("integration {name:?} declares no `[launch]` command");
        }
        other => error!("launch failed: {other}"),
    }
    ExitCode::FAILURE
}
