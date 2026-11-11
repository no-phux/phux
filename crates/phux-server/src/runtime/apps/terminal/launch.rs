//! What a Terminal runs. The host stores a [`LaunchCommand`] without looking
//! inside it; every decision about argv, shell, cwd, and `TERM` is here.

use crate::state::{ServerState, SharedState};

use phux_terminal_api::launch as engine_launch;
/// How a pane's process starts. Opaque to the host.
pub(crate) use phux_terminal_api::launch::LaunchCommand;

/// A command from a non-empty request argv.
pub(crate) fn argv_command(argv: Option<Vec<String>>) -> Option<LaunchCommand> {
    let mut argv = argv?.into_iter();
    let mut builder = LaunchCommand::new(argv.next()?);
    for arg in argv {
        builder.arg(arg);
    }
    Some(builder)
}

/// The guardian that hosts a native agent session: `<executable> agent host`
/// with the caller's cwd and environment and its startup variable.
pub(crate) fn native_host_command<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(
    executable: impl AsRef<std::ffi::OsStr>,
    cwd: Option<impl AsRef<std::ffi::OsStr>>,
    env: impl IntoIterator<Item = (K, V)>,
    (startup_key, startup): (&str, &str),
) -> LaunchCommand {
    let mut command = LaunchCommand::new(executable);
    command.args(["agent", "host"]);
    if let Some(cwd) = cwd {
        command.cwd(cwd);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command.env(startup_key, startup);
    command
}

/// Tell a guardian which delegated socket, agent, and activation it serves.
pub(crate) fn bind_native_host(
    command: &mut LaunchCommand,
    socket: impl AsRef<std::ffi::OsStr>,
    agent: &str,
    activation: &str,
    binding: &str,
) {
    command.env("PHUX_SOCKET", socket);
    command.env("PHUX_NATIVE_AGENT_ID", agent);
    command.env("PHUX_NATIVE_ACTIVATION", activation);
    command.env("PHUX_NATIVE_BINDING", binding);
}

/// A named session's seed command: the request argv, else the shell. An
/// invalid cwd is dropped with a warn, never failing the create.
pub(crate) fn named_session_command(
    state: &SharedState,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<&str>,
    env: std::collections::BTreeMap<String, String>,
) -> LaunchCommand {
    let (term, shell, login_shell) =
        state.with(|s| (s.term().to_owned(), s.shell().to_owned(), s.login_shell()));
    let mut seed_cmd = argv_command(command)
        .unwrap_or_else(|| engine_launch::default_shell_command(&shell, login_shell));
    engine_launch::apply_spawn_cwd(&mut seed_cmd, cwd, name);
    for (key, value) in env {
        seed_cmd.env(key, value);
    }
    engine_launch::apply_term(&mut seed_cmd, &term);
    seed_cmd
}

/// The pre-seeded pane's command (configured, else the default shell), with
/// the server-wide `TERM` applied.
pub(crate) fn seed_pane_command(
    state: &SharedState,
    configured: Option<LaunchCommand>,
) -> LaunchCommand {
    let mut cmd = configured.unwrap_or_else(|| {
        let (shell, login_shell) = state.with(|s| (s.shell().to_owned(), s.login_shell()));
        engine_launch::default_shell_command(&shell, login_shell)
    });
    let term = state.with(|s| s.term().to_owned());
    engine_launch::apply_term(&mut cmd, &term);
    cmd
}

/// Seed the new session `name` with a pane running `command` (else the
/// shell) in `cwd` with `env`; headless when the server runs without PTYs.
pub(crate) fn seed_named_session(
    state: &SharedState,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<&str>,
    env: std::collections::BTreeMap<String, String>,
    origin: crate::runtime::commands::SeedOrigin,
    root_token: &tokio_util::sync::CancellationToken,
) -> Result<phux_core::ids::ResourceId, String> {
    let (with_pty, scrollback) =
        state.with(|s| (s.attach_create_seeds_pty(), s.scrollback_limits()));
    let seed_cmd = with_pty.then(|| named_session_command(state, name, command, cwd, env));
    super::commands::seed::seed_session(state, name, seed_cmd, scrollback, root_token, None, origin)
        .map_err(|err| err.to_string())
}

/// A fresh start's pre-seeded session `name`: its pane runs `configured`
/// (else the shell), or no process at all without PTYs.
pub(crate) fn seed_initial_session(
    state: &SharedState,
    name: &str,
    with_pty: bool,
    configured: Option<LaunchCommand>,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    let seeded = if with_pty {
        let command = seed_pane_command(state, configured);
        super::commands::seed::seed_session_with_pty(state, name, command, scrollback, root_token)
    } else {
        super::commands::seed::seed_session_with_actor(state, name, scrollback, root_token)
    };
    seeded.map(|_| ()).map_err(|err| err.to_string())
}

/// The command a Terminal launch spec runs: its argv, else the default
/// shell, in its cwd, with its environment and the server's `TERM`.
pub(crate) fn spec_command(s: &ServerState, launch: &phux_core::TerminalLaunch) -> LaunchCommand {
    let launch = launch.clone();
    let mut cmd = argv_command((!launch.argv.is_empty()).then_some(launch.argv))
        .unwrap_or_else(|| engine_launch::default_shell_command(s.shell(), s.login_shell()));
    // A recorded launch directory is intent, not a hint. If it disappears
    // after validation, fail the activation rather than execute elsewhere.
    if let Some(cwd) = &launch.cwd {
        engine_launch::apply_strict_spawn_cwd(&mut cmd, cwd);
    }
    for (key, value) in launch.env {
        cmd.env(key, value);
    }
    engine_launch::apply_term(&mut cmd, s.term());
    cmd
}

/// Execute a Terminal launch only. The host already committed the resource
/// activation; the existing launch path reports process creation failures.
pub(crate) fn execute(launch: super::super::ResourceLaunch<'_>) -> Result<(), String> {
    let super::super::ResourceLaunch {
        state,
        resource,
        spec,
        root_token,
    } = launch;
    let phux_core::LaunchSpec::Terminal(spec) = spec else {
        return Err("the Terminal app received another kind's launch spec".to_owned());
    };
    let (scrollback, cmd) = state.with_mut(|s| {
        let cmd = s.attach_create_seeds_pty().then(|| spec_command(s, spec));
        let wire = s.intern_resource_wire(resource);
        super::commands::set_agent_session(s, wire, spec.restore.clone());
        (s.scrollback_limits(), cmd)
    });
    let cmd = cmd.map(|mut cmd| {
        state.with_mut(|s| {
            let _ = super::commands::register_pty_command(s, resource, &mut cmd);
        });
        cmd
    });
    super::commands::launch_pane_actor_with_failure_policy(
        state,
        resource,
        cmd,
        super::commands::DEFAULT_SPAWN_DIMS,
        scrollback,
        root_token,
        None,
        crate::runtime::commands::SpawnAttribution::default(),
        true,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}
