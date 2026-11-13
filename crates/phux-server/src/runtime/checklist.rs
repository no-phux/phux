//! `HOST_CHECKLIST`: what works on this host, answered with the `checklist`
//! document of `phux doctor --json` run against this server's own socket,
//! so the CLI and every client read one list built in one place
//! (`docs/troubleshooting.md#what-works-here`).

use std::time::Duration;

use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode};

use crate::state::SharedState;

/// How long doctor may take; its slowest probe (dialing our own listener)
/// gives up after a few seconds.
const DOCTOR_TIMEOUT: Duration = Duration::from_secs(30);

/// Answer `HOST_CHECKLIST`.
// ponytail: one doctor process per request; coalesce concurrent requests if
// clients ever poll it.
pub(super) async fn handle(state: &SharedState) -> CommandResult {
    match run(state).await {
        Ok(checklist) => CommandResult::OkWith(CommandValue::Json(checklist.to_string())),
        Err(message) => CommandResult::Error {
            code: ErrorCode::Unavailable,
            message,
        },
    }
}

async fn run(state: &SharedState) -> Result<serde_json::Value, String> {
    let (exe, socket) = state.with(|s| {
        (
            s.native_host_executable(),
            s.server_socket_path().map(std::path::Path::to_path_buf),
        )
    });
    let exe = exe.map_err(|err| format!("could not find this host's phux: {err}"))?;
    let mut command = tokio::process::Command::new(exe);
    command.args(["doctor", "--json"]);
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    let run = command
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(DOCTOR_TIMEOUT, run)
        .await
        .map_err(|_| "checking this host took too long".to_owned())?
        .map_err(|err| format!("could not run phux doctor: {err}"))?;
    // Doctor exits 1 when a check fails; the document is still whole.
    checklist(&output.stdout)
}

fn checklist(stdout: &[u8]) -> Result<serde_json::Value, String> {
    let mut doc: serde_json::Value = serde_json::from_slice(stdout)
        .map_err(|_| "this host's phux doctor did not answer with JSON".to_owned())?;
    match doc.get_mut("checklist").map(serde_json::Value::take) {
        Some(checklist @ serde_json::Value::Object(_)) => Ok(checklist),
        _ => Err("this host's phux is too old to build the checklist".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::checklist;

    #[test]
    fn the_checklist_is_taken_from_the_doctor_document() {
        let doc = br#"{"schema_version":1,"ok":false,"checks":[],"checklist":{"schema_version":1,"items":[]}}"#;
        assert_eq!(
            checklist(doc).unwrap(),
            serde_json::json!({"schema_version": 1, "items": []})
        );
    }

    #[test]
    fn a_doctor_without_a_checklist_is_refused() {
        assert!(checklist(br#"{"schema_version":1,"checks":[]}"#).is_err());
        assert!(checklist(b"not json").is_err());
    }
}
