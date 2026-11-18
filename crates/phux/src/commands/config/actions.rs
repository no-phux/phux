//! Command-line adapter for one local extension action.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use phux_plugin::{ActionContext, ActionSource, PluginActionOutcome, PluginActionOutput};

pub(super) fn run(
    plugin: &str,
    action: &str,
    timeout: Option<u64>,
    cwd: Option<PathBuf>,
    target: Option<&str>,
    input: Option<&str>,
    json: bool,
) -> ExitCode {
    let context = match parse_context(target, input) {
        Ok(context) => context,
        Err(err) => {
            error!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let path = phux_config::loader::config_path();
    let request = phux_plugin::PluginActionRequest {
        plugin_id: plugin.to_owned(),
        action_id: action.to_owned(),
        timeout: timeout.map(Duration::from_secs),
        cwd,
        context,
    };
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(phux_plugin::run_configured_action_logged(&path, &request)) {
        Ok(output) => print_output(&output, json),
        Err(err) => {
            error!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn parse_context(target: Option<&str>, input: Option<&str>) -> Result<ActionContext, String> {
    let target = serde_json::from_str(target.unwrap_or("{}"))
        .map_err(|err| format!("invalid --target-json: {err}"))?;
    let input = serde_json::from_str(input.unwrap_or("null"))
        .map_err(|err| format!("invalid --input-json: {err}"))?;
    Ok(ActionContext::supplied(ActionSource::Cli, target, input))
}

fn print_output(output: &PluginActionOutput, json: bool) -> ExitCode {
    if json {
        return match serde_json::to_string_pretty(output) {
            Ok(rendered) => {
                outln!("{rendered}");
                exit_code(output)
            }
            Err(err) => {
                error!("could not render plugin action JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    out!("{}", output.stdout);
    // Plugin stderr is pass-through; do not rewrite another program's output.
    eprint!("{}", output.stderr);
    exit_code(output)
}

fn exit_code(output: &PluginActionOutput) -> ExitCode {
    match output.outcome {
        PluginActionOutcome::Completed => output
            .exit_code
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        PluginActionOutcome::TimedOut => {
            // `run`'s timeout convention (canonical table: exit_codes.rs).
            ExitCode::from(crate::exit_codes::EXIT_RUN_TIMEOUT)
        }
    }
}
