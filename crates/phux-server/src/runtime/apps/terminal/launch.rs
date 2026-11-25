//! What a Terminal runs. The host stores a [`LaunchCommand`] without looking
//! inside it; every decision about argv, shell, cwd, and `TERM` is here.

use crate::state::{ServerState, SharedState};

use phux_protocol::native_agent;
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
    command.env(native_agent::NATIVE_AGENT_ID_ENV, agent);
    command.env(native_agent::NATIVE_ACTIVATION_ENV, activation);
    command.env(native_agent::NATIVE_BINDING_ENV, binding);
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
    let (term, shell, login_shell) = state.with(|s| {
        (
            s.config().term.as_str().to_owned(),
            s.config().shell.as_str().to_owned(),
            s.config().login_shell,
        )
    });
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
        let (shell, login_shell) =
            state.with(|s| (s.config().shell.as_str().to_owned(), s.config().login_shell));
        engine_launch::default_shell_command(&shell, login_shell)
    });
    let (term, fixed) = state.with(|s| (s.config().term.clone(), s.config().fixed_cwd.clone()));
    engine_launch::apply_pane_env(&mut cmd);
    engine_launch::apply_term(&mut cmd, &term);
    engine_launch::apply_spawn_cwd(&mut cmd, fixed.as_deref(), "seed");
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
        state.with(|s| (s.config().attach_create_seeds_pty, s.config().scrollback));
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
/// shell, in its last directory (else its cwd), with its environment and
/// the server's `TERM`. A `relaunch` never repeats an agent's one-shot
/// task ([`super::resume::is_agent_task`]): it starts the shell instead.
pub(crate) fn spec_command(
    s: &ServerState,
    launch: &phux_core::TerminalLaunch,
    relaunch: bool,
) -> LaunchCommand {
    let cwd = launch.relaunch_cwd().map(std::path::Path::to_path_buf);
    let launch = launch.clone();
    // A program the user allowlisted picks up where it left off; it runs
    // as the pane's process, like any other recorded argv.
    let argv = super::resume::argv(s, &launch).unwrap_or_else(|| {
        if relaunch && super::resume::is_agent_task(&launch.argv) {
            Vec::new()
        } else {
            launch.argv
        }
    });
    let mut cmd = argv_command((!argv.is_empty()).then_some(argv)).unwrap_or_else(|| {
        engine_launch::default_shell_command(s.config().shell.as_str(), s.config().login_shell)
    });
    // A recorded launch directory is intent, not a hint. If it disappears
    // after validation, fail the activation rather than execute elsewhere.
    // The last reported directory is used only while it still exists.
    if let Some(cwd) = &cwd {
        engine_launch::apply_strict_spawn_cwd(&mut cmd, cwd);
    }
    for (key, value) in launch.env {
        cmd.env(key, value);
    }
    engine_launch::apply_term(&mut cmd, s.config().term.as_str());
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
        // A later activation of the same record is a relaunch.
        let relaunch = s
            .registry()
            .record(resource)
            .and_then(|record| record.activation.as_ref())
            .is_some_and(|activation| activation.id != phux_core::ActivationId::FIRST);
        let cmd = s
            .config()
            .attach_create_seeds_pty
            .then(|| spec_command(s, spec, relaunch));
        let wire = s.idspace.intern_resource(resource);
        super::commands::set_agent_session(s, wire.clone(), spec.restore.clone());
        super::notify_mode::install(s, &wire, spec);
        super::parent::install(s, &wire, spec);
        (s.config().scrollback, cmd)
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
