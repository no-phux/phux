//! `phux doctor --remote HOST`: another machine's "What works here", asked of
//! its server (`HOST_CHECKLIST`, the list a phone reads). Each fix is shown as
//! one command to run from here: `phux new --remote` runs it there in a pane
//! that stays open on a shell, so its output can be read.

use std::process::ExitCode;

use phux_client::org::OrgError;

use super::checklist;
use crate::commands::json_err::{self, CliError, codes};
use crate::commands::remote_target;
use crate::commands::server_target::{self, ServerSpec, ServerTarget};
use crate::commands::sh_quote;

/// Exit 0 when the list came back (an item that is off is not a failure, as
/// locally), 1 when it could not be had.
pub(crate) fn run(json: bool, spec: ServerSpec) -> ExitCode {
    let host = spec.remote.clone().unwrap_or_default();
    let (rt, target) = match spec.prepare("doctor", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let doc = match rt.block_on(fetch(&target)) {
        Ok(doc) => doc,
        Err(OrgError::Attach(err)) => return target.report_unreachable(json, &err, "doctor"),
        Err(err) => return json_err::emit(json, &refusal(&host, &err), 1),
    };
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "host": host,
            "checklist": doc,
        }));
    }
    let color = crate::output::color_enabled(std::io::IsTerminal::is_terminal(&std::io::stdout()));
    let heading = format!("What works on {host}");
    out!(
        "{}",
        checklist::render_doc(&doc, &heading, color, &|command| fix_from_here(
            &host, command
        ))
    );
    ExitCode::SUCCESS
}

/// Ask `target` for its checklist.
pub(crate) async fn fetch(target: &ServerTarget) -> Result<serde_json::Value, OrgError> {
    let mut conn = target.connect().await.map_err(OrgError::Attach)?;
    phux_client::checklist::fetch_on(&mut conn).await
}

/// `command`, a fix meant for `host`'s own shell, as one command run here.
pub(crate) fn fix_from_here(host: &str, command: &str) -> String {
    let script = format!("{command}; exec \"${{SHELL:-sh}}\" -l");
    format!(
        "phux new --remote {} -- sh -lc {}",
        sh_quote(host),
        sh_quote(&script)
    )
}

/// For `host add`: what the new host says about its agent integrations when
/// some are missing, then each fix as a command run from here. Best effort
/// and bounded; `None` when they are fine or the host can't say.
pub(crate) fn missing_agents(host: &str) -> Option<Vec<String>> {
    let rt = crate::commands::cli_runtime().ok()?;
    let entry = remote_target::find_entry(&remote_target::parse_target(host).ok()?)?;
    let server = server_target::plan_entry(&rt, &entry, "doctor").ok()?;
    let target = ServerTarget::Remote(Box::new(server));
    // The timer must be made inside the runtime, so the timeout is too.
    let doc = rt
        .block_on(async { tokio::time::timeout(ASK_TIMEOUT, fetch(&target)).await })
        .ok()?
        .ok()?;
    agent_lines(host, &doc)
}

/// The lines [`missing_agents`] prints for `host`'s checklist `doc`.
fn agent_lines(host: &str, doc: &serde_json::Value) -> Option<Vec<String>> {
    let row = doc["items"]
        .as_array()?
        .iter()
        .find(|row| row["id"] == "agent_status" && row["status"] == "off")?;
    let mut lines = vec![format!(
        "On {host}: {}. Set it up from here:",
        row["reason"].as_str()?
    )];
    for command in row["fix_command"].as_str().unwrap_or_default().lines() {
        lines.push(format!("  {}", fix_from_here(host, command)));
    }
    Some(lines)
}

/// How long `host add` waits for the new host's checklist.
const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn refusal(host: &str, err: &OrgError) -> CliError {
    match err {
        OrgError::Unsupported => CliError::new(
            codes::SERVER_TOO_OLD,
            format!("{host}'s phux server is too old to report what works there"),
            format!("update phux on {host}, then run this again"),
        ),
        err => CliError::new(
            codes::TRANSPORT,
            format!("{host} could not report what works there: {err}"),
            format!("run `phux doctor` on {host} itself"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{agent_lines, fix_from_here};

    /// Only an agent item that is off speaks, with one runnable line per fix.
    #[test]
    fn host_add_names_missing_integrations_with_their_fixes() {
        let doc = |status: &str| {
            serde_json::json!({"items": [
                {"id": "skill", "status": "off", "reason": "x", "fix_command": "y"},
                {"id": "agent_status", "status": status,
                 "reason": "no agent integration for pi", "fix_command": "pi install npm:@phux/pi"},
            ]})
        };
        assert_eq!(agent_lines("mini", &doc("ok")), None);
        let lines = agent_lines("mini", &doc("off")).expect("off speaks");
        assert_eq!(
            lines[0],
            "On mini: no agent integration for pi. Set it up from here:"
        );
        assert_eq!(
            lines[1],
            format!("  {}", fix_from_here("mini", "pi install npm:@phux/pi"))
        );
        assert_eq!(lines.len(), 2);
    }

    /// The install line runs unchanged in a POSIX shell on the host, and the
    /// pane stays on a shell afterwards so its output can be read.
    #[test]
    fn a_fix_runs_there_from_here_and_leaves_a_shell() {
        assert_eq!(
            fix_from_here("mini", "pi install npm:@phux/pi"),
            concat!(
                "phux new --remote 'mini' -- sh -lc ",
                "'pi install npm:@phux/pi; exec \"$",
                "{SHELL:-sh}\" -l'"
            )
        );
        let quoted = fix_from_here("me@box", "echo 'hi' && true");
        assert!(quoted.contains(r"echo '\''hi'\'' && true"), "{quoted}");
    }
}
