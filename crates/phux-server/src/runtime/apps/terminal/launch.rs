//! What a Terminal runs. The host stores a [`LaunchCommand`] without looking
//! inside it; every decision about argv, shell, cwd, and `TERM` is here.

use crate::state::{ServerState, SharedState};

/// How a pane's process starts. Opaque to the host.
pub(crate) type LaunchCommand = phux_terminal::pty::CommandBuilder;

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

/// A named session's seed command: the request argv, else the shell. An invalid cwd is dropped with a warn, never
/// failing the create.
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
        .unwrap_or_else(|| phux_terminal::actor::default_shell_command(&shell, login_shell));
    phux_terminal::actor::apply_spawn_cwd(&mut seed_cmd, cwd, name);
    for (key, value) in env {
        seed_cmd.env(key, value);
    }
    phux_terminal::actor::apply_term(&mut seed_cmd, &term);
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
        phux_terminal::actor::default_shell_command(&shell, login_shell)
    });
    let term = state.with(|s| s.term().to_owned());
    phux_terminal::actor::apply_term(&mut cmd, &term);
    cmd
}

/// The command a Terminal launch spec runs: its argv, else the default
/// shell, in its cwd, with its environment and the server's `TERM`.
pub(crate) fn spec_command(s: &ServerState, spec: &phux_core::LaunchSpec) -> LaunchCommand {
    let launch = match spec {
        phux_core::LaunchSpec::Terminal(t) => t.clone(),
        _ => phux_core::TerminalLaunch::default(),
    };
    let mut cmd = argv_command((!launch.argv.is_empty()).then_some(launch.argv))
        .unwrap_or_else(|| phux_terminal::actor::default_shell_command(s.shell(), s.login_shell()));
    // A recorded launch directory is intent, not a hint. If it disappears
    // after validation, fail the activation rather than execute elsewhere.
    if let Some(cwd) = &launch.cwd {
        phux_terminal::actor::apply_strict_spawn_cwd(&mut cmd, cwd);
    }
    for (key, value) in launch.env {
        cmd.env(key, value);
    }
    phux_terminal::actor::apply_term(&mut cmd, s.term());
    cmd
}
