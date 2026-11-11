//! The phux CLI and server on this computer, updated through the installed
//! CLI's own checksum-verified updater. A
//! refused handoff closes nothing: the old server keeps running.

use std::path::Path;
use std::time::Duration;

use phux_client_runtime::release::Channel;
use serde_json::Value;

use crate::launch::run_for;

const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub current: String,
    pub target: String,
    pub available: bool,
    /// False when a package manager owns the install.
    pub mutable: bool,
    pub native_command: Option<String>,
    /// The channel resolved by the CLI, including its installed-channel default.
    pub channel: Option<Channel>,
    pub protocol_current: Option<String>,
    pub protocol_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    /// The live server moved, or there was none to move.
    pub done: bool,
    /// Installed, but the handoff was refused: retry without reinstalling.
    pub retry: bool,
    pub detail: String,
}

fn json(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("phux printed no JSON: {error}"))
}

fn supported(value: &Value) -> Result<(), String> {
    match value["schema_version"].as_u64() {
        Some(1 | 2) => Ok(()),
        _ => Err("Unsupported phux update response.".to_owned()),
    }
}

pub fn parse_check(value: &Value) -> Result<Cli, String> {
    supported(value)?;
    let text = |key: &str| {
        value[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("phux update reported no {key}."))
    };
    let protocol = |key: &str| value["protocol"][key].as_str().map(str::to_owned);
    Ok(Cli {
        current: text("current_version")?,
        target: text("target_version")?,
        available: value["update_available"]
            .as_bool()
            .ok_or("phux update reported no availability.")?,
        mutable: value["install"]["mutable"].as_bool().unwrap_or(false),
        native_command: value["install"]["native_command"]
            .as_str()
            .map(str::to_owned),
        channel: value["channel"].as_str().and_then(Channel::parse),
        protocol_current: protocol("current"),
        protocol_target: protocol("target"),
    })
}

pub fn parse_install(value: &Value) -> Result<Handoff, String> {
    supported(value)?;
    let result = value["server_handoff"]["result"]
        .as_str()
        .unwrap_or("not attempted");
    Ok(Handoff {
        done: matches!(result, "upgrading" | "no_server"),
        retry: matches!(result, "refused" | "failed"),
        detail: value["server_handoff"]["detail"]
            .as_str()
            .unwrap_or(result)
            .to_owned(),
    })
}

/// Use the same channel for checks and installs. None preserves the CLI's
/// installed-channel default; Rune never edits its install metadata itself.
fn channel_args(mut args: Vec<&str>, channel: Option<Channel>) -> Vec<&str> {
    if let Some(channel) = channel {
        args.extend(["--channel", channel.as_str()]);
    }
    args
}

pub fn check(phux: &Path, channel: Option<Channel>) -> Result<Cli, String> {
    let args = channel_args(vec!["update", "--check", "--json"], channel);
    let output = run_for(phux, &args, CHECK_TIMEOUT)?;
    parse_check(&json(&output)?)
}

pub fn install(phux: &Path, socket: &Path, channel: Option<Channel>) -> Result<Handoff, String> {
    let socket = socket.to_string_lossy();
    let args = channel_args(vec!["--socket", &socket, "update", "--json"], channel);
    let output = run_for(phux, &args, INSTALL_TIMEOUT)?;
    let value = json(&output)?;
    let handoff = parse_install(&value)?;
    if value["server_handoff"]["result"] == "no_server" {
        run_for(
            phux,
            &["--socket", &socket, "server", "--ensure"],
            CHECK_TIMEOUT,
        )?;
    }
    Ok(handoff)
}

pub fn retry(phux: &Path, socket: &Path) -> Result<Handoff, String> {
    let socket = socket.to_string_lossy();
    run_for(phux, &["--socket", &socket, "upgrade"], CHECK_TIMEOUT)?;
    Ok(Handoff {
        done: true,
        retry: false,
        detail: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_reads_the_cli_contract() {
        let cli = parse_check(&json!({
            "schema_version": 2,
            "current_version": "0.1.0",
            "target_version": "0.2.0",
            "update_available": true,
            "channel": "next",
            "install": { "mutable": false, "native_command": "brew upgrade phux" },
            "protocol": { "current": "3.1", "target": "4.0" },
        }))
        .unwrap();
        assert!(cli.available && !cli.mutable);
        assert_eq!(cli.channel, Some(Channel::Next));
        assert_eq!(cli.native_command.as_deref(), Some("brew upgrade phux"));
        assert_eq!(cli.protocol_target.as_deref(), Some("4.0"));
    }

    #[test]
    fn checks_and_installs_pass_the_same_channel_to_the_selected_cli() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("rune-phux-channels-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("phux");
        // No installed CLI, server, or network: the selected executable records
        // argv, then returns the public updater contract.
        fs::write(&binary, r#"#!/bin/sh
printf '%s\n' "$@" > "$0.args"
printf '%s\n' '{"schema_version":2,"current_version":"1.0.0","target_version":"2.0.0","update_available":true,"install":{"mutable":true},"server_handoff":{"result":"upgrading"}}'
"#).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let recorded = || fs::read_to_string(dir.join("phux.args")).unwrap();
        for channel in [None, Some(Channel::Stable), Some(Channel::Next)] {
            let suffix = channel.map_or(String::new(), |c| format!("--channel\n{}\n", c.as_str()));
            assert!(check(&binary, channel).unwrap().available);
            assert_eq!(recorded(), format!("update\n--check\n--json\n{suffix}"));
            assert!(
                install(&binary, Path::new("/isolated/socket"), channel)
                    .unwrap()
                    .done
            );
            assert_eq!(
                recorded(),
                format!("--socket\n/isolated/socket\nupdate\n--json\n{suffix}")
            );
        }
        // A refused handoff retries the installed binary, never reinstalls.
        assert!(retry(&binary, Path::new("/isolated/socket")).unwrap().done);
        assert_eq!(recorded(), "--socket\n/isolated/socket\nupgrade\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn check_refuses_an_unknown_contract() {
        assert!(parse_check(&json!({ "schema_version": 3 })).is_err());
        assert!(parse_check(&json!({ "schema_version": 1 })).is_err());
    }

    #[test]
    fn install_separates_a_handoff_from_a_refusal() {
        let of = |result: &str| {
            parse_install(&json!({
                "schema_version": 2,
                "server_handoff": { "result": result, "detail": "why" },
            }))
            .unwrap()
        };
        assert!(of("upgrading").done && of("no_server").done);
        assert!(of("refused").retry && !of("refused").done);
        assert!(of("failed").retry);
        assert!(!of("skipped").done && !of("skipped").retry);
    }
}
