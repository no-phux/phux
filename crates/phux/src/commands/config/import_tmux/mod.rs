//! A one-shot onboarding adapter. Acquisition, finite translation and file
//! publication stay here; the server, protocol and config loader know no tmux.

mod apply;
mod snapshot;
mod translate;

use std::path::Path;
use std::process::ExitCode;

use super::super::json_err::{self, CliError, codes};

pub(super) fn run(
    socket: Option<&Path>,
    output: Option<&Path>,
    apply: bool,
    json: bool,
) -> ExitCode {
    match import(socket, output, apply) {
        Ok((plan, saved)) => {
            if json {
                outln!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1, "toml": plan.toml, "entries": plan.entries,
                        "notes": plan.notes, "saved": saved, "applied": apply,
                    })
                );
            } else {
                report(&plan);
                if let Some(saved) = saved {
                    outln!("saved {}", saved.display());
                    if apply {
                        outln!(
                            "Existing phux settings win. Inspect with `phux config keys` and `phux config show --layers`."
                        );
                        outln!(
                            "Undo: remove tmux-import.toml from extends, then delete the layer."
                        );
                        outln!(
                            "Bindings/status: run `phux config reload`. Mouse: reattach. Shell/history: next server start."
                        );
                    }
                } else {
                    out!("{}", plan.toml);
                }
            }
            ExitCode::SUCCESS
        }
        Err(message) => json_err::emit(
            json,
            &CliError::new(
                codes::INVALID_CONFIG,
                message,
                "start tmux with your configuration first; use --tmux-socket for another server; if saving failed, inspect the reported path and remove only a previous import you intend to replace",
            ),
            2,
        ),
    }
}

fn import(
    socket: Option<&Path>,
    output: Option<&Path>,
    apply: bool,
) -> Result<(translate::Plan, Option<std::path::PathBuf>), String> {
    // Keep acquisition failures as values until run() chooses human or JSON
    // rendering; cli_runtime() prints a human diagnostic on failure.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not create inspection runtime: {err}"))?;
    let snapshot = rt.block_on(snapshot::read(socket))?;
    let plan = translate::translate(&snapshot)?;
    let saved = if apply {
        Some(apply::install(
            &phux_config::loader::config_path(),
            &plan.toml,
        )?)
    } else if let Some(path) = output {
        apply::save_new(path, &plan.toml)?;
        Some(path.to_path_buf())
    } else {
        None
    };
    Ok((plan, saved))
}

fn report(plan: &translate::Plan) {
    for note in &plan.notes {
        info!("tmux import: {note}");
    }
    for entry in &plan.entries {
        if entry.status != "mapped" {
            warn!(
                "{}: {} — {}",
                entry.status,
                entry.source.escape_debug(),
                entry.detail.escape_debug()
            );
        }
    }
    let mapped = plan.entries.iter().filter(|e| e.status == "mapped").count();
    let approximate = plan
        .entries
        .iter()
        .filter(|e| e.status == "approximate")
        .count();
    let unsupported = plan
        .entries
        .iter()
        .filter(|e| e.status == "unsupported")
        .count();
    info!(
        "tmux import: {mapped} mapped, {approximate} approximate, {unsupported} unsupported. Preview with --json for every entry."
    );
}
