//! `phux config labs`: the feature flags in `[labs]`, and developer mode.
//! Local only: it reads and writes config.toml, and never dials a server.

use std::path::Path;
use std::process::ExitCode;

use phux_config::labs::{LABS, Lab, LabScope, lab};
use phux_config::loader as config_loader;
use phux_config::settings::{Edit, write_edit};

use super::super::config_action::{LabState, LabsAction, Switch};
use crate::commands::json_err::{CliError, codes, emit};

pub(super) fn run(action: &LabsAction) -> ExitCode {
    let path = config_loader::config_path();
    match action {
        LabsAction::List { json } => list(&path, *json),
        LabsAction::Set { id, state, json } => set(&path, id, *state, *json),
        LabsAction::Developer { state, json } => developer(&path, *state, *json),
    }
}

fn list(path: &Path, json: bool) -> ExitCode {
    let cfg = match config_loader::load_from(path) {
        Ok(cfg) => cfg,
        Err(err) => return refuse(json, codes::INVALID_CONFIG, err.to_string(), 1),
    };
    let unknown: Vec<&str> = cfg.labs.unknown().collect();
    if json {
        let labs: Vec<_> = LABS
            .iter()
            .map(|lab| {
                serde_json::json!({
                    "id": lab.id,
                    "title": lab.title,
                    "detail": lab.detail,
                    "scope": lab.scope,
                    "value": cfg.labs.get(lab.id),
                    "default": lab.default,
                    "state": state_word(cfg.labs.get(lab.id)),
                })
            })
            .collect();
        outln!(
            "{}",
            serde_json::json!({
                "schema_version": 1,
                "config": path,
                "developer_mode": cfg.developer.mode,
                "labs": labs,
                "unknown": unknown,
            })
        );
        return ExitCode::SUCCESS;
    }
    outln!(
        "developer mode: {}",
        if cfg.developer.mode { "on" } else { "off" }
    );
    let width = LABS
        .iter()
        .map(|lab| lab.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    outln!("{:<width$}  STATE    SCOPE   TITLE", "ID");
    for lab in LABS {
        outln!(
            "{:<width$}  {:<7}  {:<6}  {}",
            lab.id,
            state_word(cfg.labs.get(lab.id)),
            lab.scope.as_str(),
            lab.title,
        );
    }
    for id in unknown {
        warn!("[labs] {id} is not a lab; it does nothing");
    }
    ExitCode::SUCCESS
}

/// `on`, `off`, or `default` when the file leaves it unset.
const fn state_word(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "on",
        Some(false) => "off",
        None => "default",
    }
}

fn set(path: &Path, id: &str, state: LabState, json: bool) -> ExitCode {
    let Some(lab) = lab(id) else {
        let ids: Vec<&str> = LABS.iter().map(|lab| lab.id).collect();
        return refuse(
            json,
            codes::USAGE_ERROR,
            format!("no lab is called `{id}`; labs are {}", ids.join(", ")),
            2,
        );
    };
    let value = match state {
        LabState::On => Some(true),
        LabState::Off => Some(false),
        LabState::Default => None,
    };
    let key = format!("labs.{}", lab.id);
    if let Err(code) = write(path, &key, value, json) {
        return code;
    }
    saved(json, &key, value, applies(lab))
}

fn developer(path: &Path, state: Switch, json: bool) -> ExitCode {
    let on = state == Switch::On;
    if let Err(code) = write(path, "developer.mode", Some(on), json) {
        return code;
    }
    saved(
        json,
        "developer.mode",
        Some(on),
        "Clients show Labs in Settings the next time they open it.",
    )
}

/// Set `key` to `value`, or remove it for `None`.
fn write(path: &Path, key: &str, value: Option<bool>, json: bool) -> Result<(), ExitCode> {
    let edit = value.map_or(Edit::Unset, |on| Edit::Set(toml::Value::Boolean(on)));
    write_edit(path, key, edit)
        .map(drop)
        .map_err(|err| refuse(json, codes::INVALID_CONFIG, err.to_string(), 1))
}

/// When a change to `lab` shows, by where it acts.
const fn applies(lab: &Lab) -> &'static str {
    match lab.scope {
        LabScope::Client => "Each client applies it on its next attach.",
        LabScope::Server => {
            "Run `phux config reload`, or restart the server, for the server to apply it."
        }
        LabScope::System => {
            "Clients apply it on their next attach; run `phux config reload` for the server."
        }
    }
}

fn saved(json: bool, key: &str, value: Option<bool>, applies: &str) -> ExitCode {
    if json {
        outln!(
            "{}",
            serde_json::json!({
                "schema_version": 1,
                "key": key,
                "value": value,
                "message": applies,
            })
        );
    } else {
        outln!("{key} = {}", state_word(value));
        outln!("{applies}");
    }
    ExitCode::SUCCESS
}

fn refuse(json: bool, code: &'static str, message: String, exit: u8) -> ExitCode {
    emit(
        json,
        &CliError::new(
            code,
            message,
            "run `phux config labs list` and `phux config check`; nothing was changed",
        ),
        exit,
    )
}
