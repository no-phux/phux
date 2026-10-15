//! Native launch is an explicit authenticated transaction, not shell typing.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use phux_plugin::{LaunchableIntegration, ResolvedLaunch};
use phux_protocol::native_agent::SpawnNativeAgent;

use crate::commands::cli_runtime;

pub(super) fn availability(item: &LaunchableIntegration) -> Option<String> {
    item.native.as_ref()?;
    if !item.missing_executables.is_empty() {
        return Some(format!(
            "Missing executing-host executable(s): {}",
            item.missing_executables.join(", ")
        ));
    }
    None
}

pub(super) fn spawn(
    resolved: &ResolvedLaunch,
    session: Option<&str>,
    socket: Option<&Path>,
    json: bool,
) -> ExitCode {
    let Some(native) = resolved.native.as_ref() else {
        error!("integration has no explicit native SDK launch");
        return ExitCode::from(2);
    };
    let Some(session) = session.filter(|name| !name.is_empty()) else {
        error!(
            "native launch requires --session NAME; it never chooses or creates a workspace implicitly"
        );
        return ExitCode::from(2);
    };
    let spec = SpawnNativeAgent {
        session: session.to_owned(),
        backend: native.backend.as_str().to_owned(),
        provider: native.provider.clone(),
        command: resolved.argv.clone(),
        cwd: Some(resolved.cwd.display().to_string()),
        env: BTreeMap::new(),
        native_id: None,
        guardian: None,
        guardian_key: None,
        guardian_expected: None,
    };
    let socket = socket.map_or_else(phux_server::runtime::default_socket_path, Path::to_owned);
    let runtime = match cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    runtime.block_on(async {
        match phux_client::native_agent::spawn(&socket, spec).await {
            Ok(spawned) => {
                let terminal_id = match spawned.parent {
                    phux_protocol::ids::ResourceId::Local { id } | phux_protocol::ids::ResourceId::Satellite { id, .. } => id,
                };
                let agent = phux_client::selector::format_terminal_id(&spawned.agent);
                if json {
                    crate::output::json(&serde_json::json!({
                        "schema_version":1, "terminal_id":terminal_id,
                        "agent_session_id":agent,"activation":spawned.activation,
                        "integration":resolved.integration_id,"native":true,"ready":false,
                    }))
                } else {
                    outln!("Native {} created: @{terminal_id}, agent {agent}; awaiting actual SDK readiness", resolved.integration_id);
                    ExitCode::SUCCESS
                }
            }
            Err(error) => { error!("native launch refused: {}", launch_error(error)); ExitCode::FAILURE }
        }
    })
}

fn launch_error(error: phux_client::native_agent::ClientError) -> String {
    use phux_client::native_agent::ClientError;
    match error {
        ClientError::Transport(error) => error.to_string(),
        ClientError::Invalid(message) | ClientError::Protocol(message) => message,
        ClientError::Refused { code, message } => format!("{code:?}: {message}"),
        error => error.to_string(),
    }
}
