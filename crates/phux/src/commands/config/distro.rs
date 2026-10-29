//! Local distro discovery and atomic selection. Deliberately no socket path.

use std::path::Path;
use std::process::ExitCode;

use phux_config::distro::{
    current_distro, list_distros_in, resolve_distro_in, search_dirs, switch_distro,
};

use super::super::config_action::DistroAction;

const RELOAD: &str = "Saved locally; run `phux config reload` for running clients. Server startup settings apply on the next server start.";

pub(super) fn run(action: &DistroAction) -> ExitCode {
    let json = match action {
        DistroAction::List { json }
        | DistroAction::Current { json }
        | DistroAction::Use { json, .. }
        | DistroAction::Off { json } => *json,
    };
    match execute(action, json) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => crate::commands::json_err::emit(
            json,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::INVALID_CONFIG,
                message,
                "check the distro path and `phux config check`; no selection was changed",
            ),
            1,
        ),
    }
}

fn execute(action: &DistroAction, json: bool) -> Result<(), String> {
    let config = phux_config::loader::config_path();
    let dirs = search_dirs(super::CHECKOUT_DISTROS.get().copied());
    match action {
        DistroAction::List { .. } => print_list(&dirs, json)?,
        DistroAction::Current { .. } => {
            let distro = current_distro(&config)?;
            print_selection(&config, distro.as_deref(), json, false)?;
        }
        DistroAction::Use { spec, .. } => {
            let layer = resolve_distro_in(spec, &dirs).map_err(|err| err.to_string())?;
            switch_distro(&config, Some(&layer))?;
            print_selection(&config, Some(&layer), json, true)?;
        }
        DistroAction::Off { .. } => {
            switch_distro(&config, None)?;
            print_selection(&config, None, json, true)?;
        }
    }
    Ok(())
}

fn print_list(dirs: &[std::path::PathBuf], json: bool) -> Result<(), String> {
    let distros = list_distros_in(dirs).map_err(|err| err.to_string())?;
    if json {
        return print_json(
            &serde_json::json!({"schema_version": 1, "distros": distros, "search_dirs": dirs}),
        );
    }
    for distro in distros {
        outln!("{}\t{}", distro.name, distro.path.display());
    }
    Ok(())
}

fn print_selection(
    config: &Path,
    distro: Option<&Path>,
    json: bool,
    saved: bool,
) -> Result<(), String> {
    if json {
        return print_json(&serde_json::json!({
            "schema_version": 1, "config": config, "distro": distro,
            "reload_required": saved, "message": if saved { RELOAD } else { "Configured selection; running clients may differ." }
        }));
    }
    outln!(
        "{}",
        distro.map_or_else(|| "off".to_owned(), |path| path.display().to_string())
    );
    if saved {
        outln!("{RELOAD}");
    }
    Ok(())
}

fn print_json(value: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|err| err.to_string())?;
    outln!("{text}");
    Ok(())
}
