//! Internal direct-exec guardian entry. Only the authenticated native spawn
//! transaction supplies its precreated resource, activation, and binding.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_agent_host::session::HostSpec;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::SpawnNativeAgent;

pub(super) fn run() -> ExitCode {
    let spec = match bootstrap() {
        Ok(spec) => spec,
        Err(message) => {
            error!("native host bootstrap refused: {message}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match crate::commands::cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match runtime.block_on(phux_agent_host::session::run(spec)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!("native agent host failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!("missing server-provided {name}; launch through `phux launch --acp`")
        })
}

fn local_resource(name: &str) -> Result<ResourceId, String> {
    let value = required(name)?;
    let number = value.strip_prefix('@').unwrap_or(&value);
    let id = number
        .parse::<u32>()
        .map_err(|_| format!("invalid local resource in {name}"))?;
    if id == 0 {
        return Err(format!("zero resource in {name}"));
    }
    Ok(ResourceId::Local { id })
}

fn host_cwd(cwd: Option<String>) -> Result<PathBuf, String> {
    cwd.map(PathBuf::from).map_or_else(
        || std::env::current_dir().map_err(|error| error.to_string()),
        Ok,
    )
}

fn host_environment(
    extra: BTreeMap<String, String>,
    socket: &std::path::Path,
    parent: &ResourceId,
) -> Result<BTreeMap<String, String>, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.extend(extra);
    // Worker model tools use the original restricted socket, not any profile
    // override or a production socket inherited from a frontend.
    env.insert("PHUX_SOCKET".into(), socket.display().to_string());
    env.insert(
        "PHUX_TERMINAL_ID".into(),
        phux_client::selector::format_terminal_id(parent),
    );
    env.insert("PHUX_AGENT_REPORTING".into(), "0".into());
    env.insert(
        "PHUX_BIN".into(),
        std::env::current_exe()
            .map_err(|error| error.to_string())?
            .display()
            .to_string(),
    );
    for name in ["PHUX_WS_TOKENS", "PHUX_WS_TLS_CERT", "PHUX_WS_TLS_KEY"] {
        env.remove(name);
    }
    Ok(env)
}

fn bootstrap() -> Result<HostSpec, String> {
    let public: SpawnNativeAgent = serde_json::from_str(&required("PHUX_NATIVE_HOST_SPEC")?)
        .map_err(|error| format!("invalid native spawn configuration: {error}"))?;
    let socket = PathBuf::from(required("PHUX_SOCKET")?);
    let parent = local_resource("PHUX_TERMINAL_ID")?;
    let agent = local_resource("PHUX_NATIVE_AGENT_ID")?;
    let activation = required("PHUX_NATIVE_ACTIVATION")?;
    let binding_proof = required("PHUX_NATIVE_BINDING")?;
    let cwd = host_cwd(public.cwd)?;
    let env = host_environment(public.env, &socket, &parent)?;
    let command = public.command;
    Ok(HostSpec {
        backend: public.backend,
        provider: public.provider,
        command,
        cwd,
        env,
        socket,
        parent,
        agent,
        activation,
        binding_proof,
        native_id: public.native_id,
    })
}
