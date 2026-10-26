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

pub fn runtime_check(phux: &Path, channel: Channel) -> anyhow::Result<RuntimeRelease> {
    let phux = crate::launch::command_path(phux)?;
    let text = run_bounded(
        phux_command(&phux).args(["update", "--check", "--channel", channel.as_str(), "--json"]),
        None,
        CHECK_TIMEOUT,
        "phux release check timed out. Nothing was installed; try Check now again.",
    )?;
    runtime_release(&serde_json::from_str(&text)?)
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
        if cfg!(windows) {
            "phux update timed out. Check the installed version in WSL before retrying; Desktop did not close any agent sessions."
        } else {
            "phux update timed out. The previous binaries stay in place; try again."
        },
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
        #[cfg(windows)]
        result.message.push_str(
            " Use Retry server handoff in Desktop to retry in the selected WSL distribution. No agent sessions were closed.",
        );
        #[cfg(not(windows))]
        result.message.push_str(&format!(
            " Retry the handoff with {} --socket {} upgrade. No agent sessions were closed.",
            crate::attach::quote_paths(std::slice::from_ref(&phux)),
            crate::attach::quote_paths(std::slice::from_ref(&socket))
        ));
    }
    Ok(result)
}

fn runtime_socket(phux: &Path, socket: &Path) -> anyhow::Result<PathBuf> {
    if socket.as_os_str().is_empty() {
        crate::launch::ensure_server(phux, None)
    } else {
        Ok(socket.to_path_buf())
    }
}

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
