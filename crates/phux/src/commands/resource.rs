//! `phux resource show|wait|methods` (PHA-406): the resource noun.
//!
//! Each verb is a thin projection over `phux_client::resource`, which owns
//! the wire work and the `--json` documents the MCP adapter shares. This
//! file owns TARGET resolution, the exit-code mapping, and the human text.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::deadline::Deadline;
use phux_client::resource::LookupError;
use phux_client::resource::cursor::Cursor;
use phux_client::resource::methods::ResourceMethods;
use phux_client::resource::show::ResourceDetails;
use phux_client::resource::wait::{ResourceWait, ResourceWaitError, WaitFailure, WaitOutcome};
use phux_protocol::ids::ResourceId;
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::{ResourceAction, cli_runtime, parse_selector, resolve_target};
use crate::exit_codes::{EXIT_FAILURE, EXIT_PARTIAL_VIEW, EXIT_USAGE, EXIT_WAIT_TIMEOUT};
use crate::selector::Selector;

/// `phux resource <action>`.
pub(crate) fn run_resource(action: &ResourceAction, socket: Option<PathBuf>) -> ExitCode {
    let socket = socket.unwrap_or_else(default_socket_path);
    match action {
        ResourceAction::Show { json, target } => run_show(&socket, target, json.json),
        ResourceAction::Wait {
            timeout,
            after,
            json,
            target,
        } => run_wait(&socket, target, *timeout, after.as_deref(), json.json),
        ResourceAction::Methods { json, target } => run_methods(&socket, target, json.json),
    }
}

/// Resolve TARGET to one resource. A direct id (`@N`, `host/@N`) names
/// itself without a lookup, so a wait can address a resource that has
/// already closed; any other selector resolves against the live inventory
/// like every other one-pane verb.
async fn resolve(
    socket: &Path,
    target: &str,
    verb: &str,
    json: bool,
) -> Result<ResourceId, ExitCode> {
    let selector = parse_selector(Some(target))?;
    if let Some(id) = direct_id(&selector) {
        return Ok(id);
    }
    resolve_target(socket, &selector, verb, json).await
}

pub(crate) fn direct_id(selector: &Selector) -> Option<ResourceId> {
    match selector {
        Selector::ResourceId(id) => Some(ResourceId::local(*id)),
        Selector::SatelliteResourceId { host, id } => {
            Some(ResourceId::satellite(host.as_str(), *id))
        }
        _ => None,
    }
}

fn run_show(socket: &Path, target: &str, json: bool) -> ExitCode {
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async {
        let id = match resolve(socket, target, "resource show", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match phux_client::resource::show::show(socket, &id).await {
            Ok(details) => print_show(&details, json),
            Err(err) => report_lookup_error(json, socket, "resource show", &err),
        }
    })
}

fn run_methods(socket: &Path, target: &str, json: bool) -> ExitCode {
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async {
        let id = match resolve(socket, target, "resource methods", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match phux_client::resource::methods::methods(socket, &id).await {
            Ok(report) => print_methods(&report, json),
            Err(err) => report_lookup_error(json, socket, "resource methods", &err),
        }
    })
}

fn run_wait(
    socket: &Path,
    target: &str,
    timeout: Option<u64>,
    after: Option<&str>,
    json: bool,
) -> ExitCode {
    let after = match after.map(str::parse::<Cursor>).transpose() {
        Ok(after) => after,
        Err(err) => return invalid_cursor(json, &err),
    };
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async {
        let deadline = Deadline::new(timeout.map(Duration::from_secs));
        // Resolution shares the budget but always gets the first-read floor,
        // so `--timeout 0` still answers once, as `phux wait` does.
        let first_read = deadline.floored(phux_client::wait::FIRST_READ_FLOOR);
        let id = match first_read
            .run(resolve(socket, target, "resource wait", json))
            .await
        {
            Some(Ok(id)) => id,
            Some(Err(code)) => return code,
            None => {
                eprintln!("phux: resource wait timed out resolving {target}");
                return ExitCode::from(EXIT_WAIT_TIMEOUT);
            }
        };
        let wait =
            phux_client::resource::wait::wait_for_exit(socket, id, after.as_ref(), first_read)
                .await;
        match wait {
            Ok(wait) => report_wait(&wait, json),
            Err(err) => report_wait_failure(json, socket, &err),
        }
    })
}

pub(crate) fn invalid_cursor(json: bool, err: &dyn std::fmt::Display) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(
            codes::INVALID_CURSOR,
            err.to_string(),
            "pass the `cursor` a previous `phux resource wait` or `phux watch` printed, \
             or omit --after",
        ),
        EXIT_USAGE,
    )
}

fn report_wait(wait: &ResourceWait, json: bool) -> ExitCode {
    if wait.cursor_void {
        eprintln!(
            "phux: resource wait: the --after cursor belongs to another server run; \
             answered from the resource's current state instead"
        );
    }
    let code = if json {
        print_document(&wait.to_json())
    } else {
        print_wait_human(wait)
    };
    if code != ExitCode::SUCCESS {
        return code;
    }
    ExitCode::from(wait_exit_code(wait.outcome))
}

const fn wait_exit_code(outcome: WaitOutcome) -> u8 {
    match outcome {
        WaitOutcome::Exited => crate::exit_codes::EXIT_SUCCESS,
        WaitOutcome::Gone => EXIT_FAILURE,
        WaitOutcome::TimedOut => EXIT_WAIT_TIMEOUT,
    }
}

fn print_wait_human(wait: &ResourceWait) -> ExitCode {
    let resource = crate::selector::format_terminal_id(&wait.resource);
    let resume = wait
        .cursor
        .as_ref()
        .map(|cursor| format!("; resume with --after {cursor}"))
        .unwrap_or_default();
    match wait.outcome {
        WaitOutcome::Exited => outln!("{resource} exited: {}", describe_exit(wait)),
        WaitOutcome::Gone => {
            eprintln!(
                "phux: {resource} is gone: it is not in the inventory and no event reported \
                 how it ended"
            );
            eprintln!(
                "  spawn with `phux spawn --retain` to keep an exited pane inspectable, or \
                 pass --after with an earlier cursor to replay a close the server still holds"
            );
        }
        WaitOutcome::TimedOut => eprintln!(
            "phux: resource wait timed out after {}s{resume}",
            wait.waited.as_secs()
        ),
    }
    ExitCode::SUCCESS
}

fn describe_exit(wait: &ResourceWait) -> String {
    let exit = wait.exit.clone().unwrap_or_default();
    let how = match (exit.status, exit.signal) {
        (Some(status), _) => format!("status {status}"),
        (None, Some(signal)) => format!("signal {signal}"),
        (None, None) => "status unknown".to_owned(),
    };
    let retained = if wait.retained { " (retained)" } else { "" };
    format!("{how}{retained}")
}

fn print_show(details: &ResourceDetails, json: bool) -> ExitCode {
    let doc = details.to_json();
    if json {
        return print_document(&doc);
    }
    for key in [
        "resource",
        "kind",
        "parent",
        "session",
        "title",
        "cwd",
        "lifecycle",
        "exit",
        "input_holder",
        "process",
        "tags",
        "agent",
        "agent_session",
    ] {
        if let Some(line) = human_field(&doc, key) {
            outln!("{line}");
        }
    }
    warn_unreachable(&details.unreachable);
    ExitCode::SUCCESS
}

/// `key: value` for a non-null field; strings unquoted, the rest compact
/// JSON.
fn human_field(doc: &serde_json::Value, key: &str) -> Option<String> {
    let value = doc.get(key).filter(|value| !value.is_null())?;
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    Some(format!("{key}: {text}"))
}

fn print_methods(report: &ResourceMethods, json: bool) -> ExitCode {
    if json {
        return print_document(&report.to_json());
    }
    for judged in &report.methods {
        let entry = judged.to_json();
        let facet = entry["facet"].as_str().unwrap_or_default();
        let verb = entry["verb"].as_str().unwrap_or_default();
        let state = judged.unavailable.map_or_else(
            || "available".to_owned(),
            |why| format!("unavailable ({})", why.as_str()),
        );
        outln!("{:<28}{facet:<15}{verb:<22}{state}", judged.method.name);
    }
    ExitCode::SUCCESS
}

fn warn_unreachable(notices: &[String]) {
    for message in notices {
        eprintln!("phux: warning: partial results — {message}");
    }
}

fn report_lookup_error(json: bool, socket: &Path, verb: &str, err: &LookupError) -> ExitCode {
    match err {
        LookupError::Attach(err) => json_err::report_no_server(json, err, socket, verb),
        LookupError::NotFound { unreachable, .. } if unreachable.is_empty() => json_err::emit(
            json,
            &CliError::new(
                codes::NO_SUCH_TARGET,
                err.to_string(),
                "`phux ls --json` lists the resources this server holds",
            ),
            EXIT_FAILURE,
        ),
        LookupError::NotFound { unreachable, .. } => partial_view(json, unreachable),
    }
}

fn partial_view(json: bool, notices: &[String]) -> ExitCode {
    json_err::emit(json, &partial_view_error(notices), EXIT_PARTIAL_VIEW)
}

fn partial_view_error(notices: &[String]) -> CliError {
    CliError::new(
        codes::PARTIAL_VIEW,
        format!(
            "the resource is not in this server's view, but the view is incomplete ({})",
            notices.join("; ")
        ),
        "retry once the federation link is back; the resource may exist",
    )
}

/// Report a wait that could not answer, naming the cursor it reached so a
/// wait against the same server run can resume instead of starting over.
fn report_wait_failure(json: bool, socket: &Path, err: &ResourceWaitError) -> ExitCode {
    let (mut failure, code) = match &err.cause {
        WaitFailure::Attach(cause) => (
            json_err::no_server_error(cause, socket, "resource wait"),
            EXIT_FAILURE,
        ),
        WaitFailure::PartialView(notices) => (partial_view_error(notices), EXIT_PARTIAL_VIEW),
        WaitFailure::Denied(message) => (
            CliError::new(
                codes::PERMISSION_DENIED,
                format!(
                    "the server refused to let this connection observe the resource: {message}"
                ),
                "the connection's scope must allow observing this resource to wait on it",
            ),
            EXIT_USAGE,
        ),
    };
    if let Some(cursor) = &err.cursor {
        failure.remedy = format!("{}; resume with --after {cursor}", failure.remedy);
    }
    json_err::emit(json, &failure, code)
}

fn print_document(doc: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(doc) {
        Ok(text) => {
            outln!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            true,
            &CliError::new(codes::JSON_SERIALIZE, err.to_string(), ""),
            EXIT_FAILURE,
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> ResourceAction {
        let cli = crate::parse_cli(argv).expect("invocation should parse");
        let Some(crate::commands::Command::Resource { action }) = cli.command else {
            panic!("expected the resource noun");
        };
        action
    }

    #[test]
    fn wait_takes_flags_before_the_target() {
        let ResourceAction::Wait {
            timeout,
            after,
            json,
            target,
        } = parse(&[
            "phux",
            "resource",
            "wait",
            "--timeout",
            "30",
            "--after",
            "ab:4",
            "--json",
            "@7",
        ])
        else {
            panic!("expected wait");
        };
        assert_eq!(timeout, Some(30));
        assert_eq!(after.as_deref(), Some("ab:4"));
        assert!(json.json);
        assert_eq!(target, "@7");
    }

    #[test]
    fn show_and_methods_parse() {
        assert!(matches!(
            parse(&["phux", "resource", "show", "--json", "@3"]),
            ResourceAction::Show { .. }
        ));
        assert!(matches!(
            parse(&["phux", "resource", "methods", "work:1.0"]),
            ResourceAction::Methods { .. }
        ));
    }

    #[test]
    fn a_direct_id_is_taken_as_given() {
        let selector = crate::selector::parse("@9").expect("selector");
        assert_eq!(direct_id(&selector), Some(ResourceId::local(9)));
        let satellite = crate::selector::parse("edge/@4").expect("selector");
        assert_eq!(
            direct_id(&satellite),
            Some(ResourceId::satellite("edge", 4))
        );
        let session = crate::selector::parse("work").expect("selector");
        assert_eq!(direct_id(&session), None);
    }

    #[test]
    fn wait_outcomes_map_to_the_documented_exit_codes() {
        assert_eq!(wait_exit_code(WaitOutcome::Exited), 0);
        assert_eq!(wait_exit_code(WaitOutcome::Gone), 1);
        assert_eq!(wait_exit_code(WaitOutcome::TimedOut), 124);
    }

    #[test]
    fn a_malformed_cursor_is_a_usage_error_before_any_connection() {
        let socket = std::path::Path::new("/nonexistent/phux.sock");
        let code = run_wait(socket, "@1", Some(1), Some("not-a-cursor"), true);
        assert_eq!(code, ExitCode::from(EXIT_USAGE));
    }
}
