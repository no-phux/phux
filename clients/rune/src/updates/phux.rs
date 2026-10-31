//! The phux CLI and server on this computer, updated through the installed
//! CLI's own checksum-verified updater, as the GPUI desktop does it. A
//! refused handoff closes nothing: the old server keeps running.

use std::path::Path;
use std::time::Duration;

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

/// With no `--channel`, the CLI follows the channel it was installed from.
pub fn check(phux: &Path) -> Result<Cli, String> {
    let output = run_for(phux, &["update", "--check", "--json"], CHECK_TIMEOUT)?;
    parse_check(&json(&output)?)
}

pub fn install(phux: &Path, socket: &Path) -> Result<Handoff, String> {
    let socket = socket.to_string_lossy();
    let output = run_for(
        phux,
        &["--socket", &socket, "update", "--json"],
        INSTALL_TIMEOUT,
    )?;
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
            "install": { "mutable": false, "native_command": "brew upgrade phux" },
            "protocol": { "current": "3.1", "target": "4.0" },
        }))
        .unwrap();
        assert!(cli.available && !cli.mutable);
        assert_eq!(cli.native_command.as_deref(), Some("brew upgrade phux"));
        assert_eq!(cli.protocol_target.as_deref(), Some("4.0"));
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
