//! The phux CLI and server on this computer: release checks, installs and
//! the live server's handoff, all through the installed CLI's own updater.

use super::*;

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
    let text = run_bounded(
        Command::new(phux).args(["update", "--check", "--channel", channel.as_str(), "--json"]),
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
    let text = run_bounded(
        Command::new(phux).arg("--socket").arg(socket).args([
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
    if value["server_handoff"]["result"] == "no_server" {
        output(
            Command::new(phux)
                .arg("--socket")
                .arg(socket)
                .args(["server", "--ensure"]),
        )
        .context("phux was installed but the server could not start")?;
    }
    if !result.reconnect {
        result.message.push_str(&format!(
            " Retry the handoff with {} --socket {} upgrade. No agent sessions were closed.",
            crate::attach::quote_paths(&[phux.to_path_buf()]),
            crate::attach::quote_paths(&[socket.to_path_buf()])
        ));
    }
    Ok(result)
}

pub fn runtime_retry(phux: &Path, socket: &Path) -> anyhow::Result<RuntimeInstalled> {
    output(
        Command::new(phux)
            .arg("--socket")
            .arg(socket)
            .arg("upgrade")
            .stdin(Stdio::null()),
    )?;
    Ok(RuntimeInstalled {
        message: "Server handoff requested; reconnecting windows.".into(),
        reconnect: true,
        retry_handoff: false,
    })
}
