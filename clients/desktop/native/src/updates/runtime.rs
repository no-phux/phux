//! The phux CLI and server on this computer: release checks, installs and
//! the live server's handoff, all through the installed CLI's own updater.

use super::*;

#[cfg(windows)]
use crate::platform::phux_command;

#[cfg(not(windows))]
fn phux_command(phux: &Path) -> Command {
    Command::new(phux)
}

#[derive(Clone, Debug)]
pub struct RuntimeRelease {
    pub current: String,
    pub target: String,
    pub available: bool,
    pub mutable: bool,
    pub native_command: Option<String>,
    /// The installed phux's wire protocol; absent from older CLIs.
    pub protocol_current: Option<String>,
    /// The target release's wire protocol, when the CLI knows it.
    pub protocol_target: Option<String>,
}

pub fn runtime_check(
    phux: &Path,
    socket: &Path,
    channel: Channel,
) -> anyhow::Result<RuntimeRelease> {
    let phux = crate::launch::command_path(phux)?;
    let text = run_bounded(
        phux_command(&phux).args(["update", "--check", "--channel", channel.as_str(), "--json"]),
        None,
        CHECK_TIMEOUT,
        "phux release check timed out. Nothing was installed; try Check now again.",
    )?;
    let runtime = runtime_release(&serde_json::from_str(&text)?)?;
    #[cfg(windows)]
    {
        let command = manual_runtime_update(phux_command(&phux), socket, channel);
        Ok(windows_runtime_policy(runtime, &command))
    }
    #[cfg(not(windows))]
    {
        let _ = socket;
        Ok(runtime)
    }
}

/// Cancelling native wsl.exe does not prove its Linux updater stopped. Until
/// coordinator-side cancellation exists, Desktop only checks WSL releases.
#[cfg(any(windows, test))]
pub(super) fn windows_runtime_policy(
    mut runtime: RuntimeRelease,
    command: &Command,
) -> RuntimeRelease {
    runtime.mutable = false;
    runtime.native_command = Some(powershell_command(command));
    runtime
}

#[cfg(any(windows, test))]
pub(super) fn manual_runtime_update(
    mut command: Command,
    socket: &Path,
    channel: Channel,
) -> Command {
    command
        .arg("--socket")
        .arg(socket)
        .args(["update", "--channel", channel.as_str()]);
    command
}

/// PowerShell single-quoted literals keep distro names, CLI paths and sockets
/// as arguments, including apostrophes and shell-looking user data.
#[cfg(any(windows, test))]
pub(super) fn powershell_command(command: &Command) -> String {
    let arguments = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|argument| format!("'{}'", argument.to_string_lossy().replace('\'', "''")))
        .collect::<Vec<_>>();
    format!("& {}", arguments.join(" "))
}

pub fn runtime_release(value: &Value) -> anyhow::Result<RuntimeRelease> {
    if value["schema_version"] != 1 && value["schema_version"] != 2 {
        bail!("Unsupported phux update response");
    }
    let text = |key: &str| value["protocol"][key].as_str().map(str::to_owned);
    Ok(RuntimeRelease {
        current: value["current_version"]
            .as_str()
            .context("Missing current version")?
            .into(),
        target: value["target_version"]
            .as_str()
            .context("Missing target version")?
            .into(),
        available: value["update_available"]
            .as_bool()
            .context("Missing update availability")?,
        mutable: value["install"]["mutable"].as_bool().unwrap_or(false),
        native_command: value["install"]["native_command"]
            .as_str()
            .map(str::to_owned),
        protocol_current: text("current"),
        protocol_target: text("target"),
    })
}

pub struct RuntimeInstalled {
    pub socket: PathBuf,
    pub message: String,
    pub reconnect: bool,
    pub retry_handoff: bool,
}

#[cfg(any(not(windows), test))]
pub fn runtime_result(value: &Value) -> anyhow::Result<RuntimeInstalled> {
    if value["schema_version"] != 1 && value["schema_version"] != 2 {
        bail!("Unsupported phux update response");
    }
    let result = value["server_handoff"]["result"]
        .as_str()
        .unwrap_or("not attempted");
    Ok(RuntimeInstalled {
        socket: PathBuf::new(),
        message: format!(
            "phux installed. Server handoff: {result}. {}",
            value["server_handoff"]["detail"].as_str().unwrap_or("")
        ),
        reconnect: matches!(result, "upgrading" | "no_server"),
        retry_handoff: matches!(result, "refused" | "failed"),
    })
}

#[cfg(windows)]
pub fn runtime_install(
    phux: &Path,
    socket: &Path,
    channel: Channel,
) -> anyhow::Result<RuntimeInstalled> {
    let command = manual_runtime_update(phux_command(phux), socket, channel);
    bail!(
        "Automatic runtime updates are disabled on Windows: cancelling wsl.exe cannot guarantee the Linux updater stopped. Run manually in PowerShell: {}",
        powershell_command(&command)
    );
}

#[cfg(not(windows))]
pub fn runtime_install(
    phux: &Path,
    socket: &Path,
    channel: Channel,
) -> anyhow::Result<RuntimeInstalled> {
    let phux = crate::launch::command_path(phux)?;
    let socket = runtime_socket(&phux, socket)?;
    let text = run_bounded(
        phux_command(&phux).arg("--socket").arg(&socket).args([
            "update",
            "--channel",
            channel.as_str(),
            "--json",
        ]),
        None,
        STAGE_TIMEOUT,
        "phux update timed out. The previous binaries stay in place; try again.",
    )?;
    let value: Value = serde_json::from_str(&text)?;
    let mut result = runtime_result(&value)?;
    result.socket = socket.clone();
    if value["server_handoff"]["result"] == "no_server" {
        run_bounded(
            phux_command(&phux)
                .arg("--socket")
                .arg(&socket)
                .args(["server", "--ensure"]),
            None,
            CHECK_TIMEOUT,
            "phux was installed but starting its server timed out. Try the server handoff again.",
        )
        .context("phux was installed but the server could not start")?;
    }
    if !result.reconnect {
        result.message.push_str(&format!(
            " Retry the handoff with {} --socket {} upgrade. No agent sessions were closed.",
            crate::attach::quote_paths(std::slice::from_ref(&phux)),
            crate::attach::quote_paths(std::slice::from_ref(&socket))
        ));
    }
    Ok(result)
}

#[cfg(not(windows))]
fn runtime_socket(phux: &Path, socket: &Path) -> anyhow::Result<PathBuf> {
    if socket.as_os_str().is_empty() {
        crate::launch::ensure_server(phux, None)
    } else {
        Ok(socket.to_path_buf())
    }
}

#[cfg(windows)]
pub fn runtime_retry(phux: &Path, socket: &Path) -> anyhow::Result<RuntimeInstalled> {
    let mut command = phux_command(phux);
    command.arg("--socket").arg(socket).arg("upgrade");
    bail!(
        "Automatic server handoff is disabled on Windows: cancelling wsl.exe cannot guarantee the Linux command stopped. Run manually in PowerShell: {}",
        powershell_command(&command)
    );
}

#[cfg(not(windows))]
pub fn runtime_retry(phux: &Path, socket: &Path) -> anyhow::Result<RuntimeInstalled> {
    let phux = crate::launch::command_path(phux)?;
    let socket = runtime_socket(&phux, socket)?;
    run_bounded(
        phux_command(&phux)
            .arg("--socket")
            .arg(&socket)
            .arg("upgrade")
            .stdin(Stdio::null()),
        None,
        CHECK_TIMEOUT,
        "Server handoff timed out. Try Retry server handoff again; no agent sessions were closed.",
    )?;
    Ok(RuntimeInstalled {
        socket,
        message: "Server handoff requested; reconnecting windows.".into(),
        reconnect: true,
        retry_handoff: false,
    })
}
