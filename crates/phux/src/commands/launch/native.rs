//! Native launch is an explicit authenticated transaction, not shell typing.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_config::integration::NativeBackend;
use phux_plugin::{LaunchableIntegration, ResolvedLaunch};
use phux_protocol::native_agent::{NativeRequest, SpawnNativeAgent, SpawnedNativeAgent};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

use crate::commands::agent::sdk_runtime;
use crate::commands::cli_runtime;

pub(super) fn availability(item: &LaunchableIntegration) -> Option<String> {
    let native = item.native.as_ref()?;
    if !item.missing_executables.is_empty() {
        return Some(format!(
            "Missing executing-host executable(s): {}",
            item.missing_executables.join(", ")
        ));
    }
    if native.backend == NativeBackend::Node {
        if let Err(message) = sdk_runtime::node_executable() {
            return Some(message);
        }
        if let Err(message) = sdk_runtime::available() {
            return Some(message);
        }
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
        sdk_runtime: None,
        node_bridge: None,
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
        match spawn_request(&socket, spec).await {
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
            Err(message) => { error!("native launch refused: {message}"); ExitCode::FAILURE }
        }
    })
}

pub(crate) async fn spawn_request(
    socket: &Path,
    spec: SpawnNativeAgent,
) -> Result<SpawnedNativeAgent, String> {
    let mut connection = Connection::connect(socket)
        .await
        .map_err(|error| error.to_string())?;
    let (result, _) = connection
        .request(
            1,
            Command::NativeAgent {
                request: Box::new(NativeRequest::Spawn { spec }),
            },
        )
        .await
        .map_err(|error| error.to_string())?
        .into_parts();
    match result {
        CommandResult::OkWith(CommandValue::Json(value)) => serde_json::from_str(&value)
            .map_err(|error| format!("invalid native spawn reply: {error}")),
        CommandResult::Error { code, message } => Err(format!("{code:?}: {message}")),
        _ => Err("server returned no native spawn transaction result".to_owned()),
    }
}
