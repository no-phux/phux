//! `MIRROR` (`docs/resource-kinds.md#standing-mirrors`): list a host's
//! capture targets and start or stop a mirror of one on demand, in the same
//! `mirror-<target>` session a `[[mirrors]]` entry gets. Linux runs the
//! mirror here; on macOS Rune's runner (`phux agent surface standing`)
//! reports the targets and runs the mirrors, so without it both are
//! refused. On-demand mirrors last until stopped or the server restarts or
//! upgrades.

use std::time::{Duration, Instant};

use phux_config::MirrorConfigEntry;
use phux_config::mirror::MirrorFormat;
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode, MirrorOp};

use crate::state::{ServerState, SharedState};

/// How long a start waits for the mirror's surface.
const SURFACE_WAIT: Duration = Duration::from_secs(20);
/// How long `phux agent surface targets` may take.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

const NO_RUNNER: &str = "on macOS Rune runs the mirrors, and it is not open on this Mac: open Rune here, then try again";

fn refuse(code: ErrorCode, message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.into(),
    }
}

fn json(value: &serde_json::Value) -> CommandResult {
    CommandResult::OkWith(CommandValue::Json(value.to_string()))
}

/// Answer one `MIRROR` addressed to this server.
pub(crate) async fn handle(state: &SharedState, op: MirrorOp) -> CommandResult {
    let macos = cfg!(target_os = "macos");
    match op {
        MirrorOp::Targets if macos => state.with(|s| {
            s.mirror_demand().fresh_targets(Instant::now()).map_or_else(
                || refuse(ErrorCode::Unavailable, NO_RUNNER),
                |targets| json(&serde_json::json!({ "targets": targets })),
            )
        }),
        MirrorOp::Targets => match list_here(state).await {
            Ok(targets) => json(&serde_json::json!({ "targets": targets })),
            Err(message) => refuse(ErrorCode::Unavailable, message),
        },
        MirrorOp::Start { target, jpeg } => start(state, target, jpeg).await,
        MirrorOp::Stop { target } => stop(state, &target),
    }
}

/// Answer Rune's macOS runner: keep the targets it reports, and name the
/// mirrors it should run.
pub(crate) fn provide(state: &SharedState, targets: &str) -> CommandResult {
    let targets: serde_json::Value = match serde_json::from_str(targets) {
        Ok(value @ serde_json::Value::Array(_)) => value,
        _ => return refuse(ErrorCode::InvalidCommand, "targets must be a JSON array"),
    };
    let mirrors = state.with_mut(|s| {
        let demand = s.mirror_demand_mut();
        demand.runner = Some((Instant::now(), targets));
        demand
            .wanted()
            .iter()
            .map(|e| serde_json::json!({ "session": e.session_name(), "args": e.args() }))
            .collect::<Vec<_>>()
    });
    json(&serde_json::json!({ "mirrors": mirrors }))
}

/// Record that `target` should be mirrored, unless it already is, and name
/// its session. Idempotent.
fn want(state: &SharedState, target: &str, jpeg: bool) -> Result<String, CommandResult> {
    let mut entry = checked(target)?;
    entry.format = jpeg.then_some(MirrorFormat::Jpeg);
    let name = entry.session_name();
    state.with_mut(|s| {
        let demand = s.mirror_demand_mut();
        if cfg!(target_os = "macos") && demand.fresh_targets(Instant::now()).is_none() {
            return Err(refuse(ErrorCode::Unavailable, NO_RUNNER));
        }
        let known = demand.config.iter().any(|e| e.session_name() == name)
            || demand.started.contains_key(&name);
        if !known {
            demand.started.insert(name.clone(), entry);
        }
        Ok(name.clone())
    })
}

/// Validate `target` as `phux agent surface mirror` will read it.
fn checked(target: &str) -> Result<MirrorConfigEntry, CommandResult> {
    let entry = MirrorConfigEntry {
        target: target.trim().to_owned(),
        format: None,
        quality: None,
        max_width: None,
        interval_ms: None,
    };
    if entry.target.is_empty()
        || entry.target.chars().any(char::is_control)
        || entry.session_name() == super::MIRROR_SESSION_PREFIX.trim_end_matches('-')
    {
        return Err(refuse(
            ErrorCode::InvalidCommand,
            "name `display`, `display:OUTPUT`, `App` or `App:title`",
        ));
    }
    Ok(entry)
}

async fn start(state: &SharedState, target: String, jpeg: bool) -> CommandResult {
    let name = match want(state, &target, jpeg) {
        Ok(name) => name,
        Err(refused) => return refused,
    };
    super::request_reload(state);
    let deadline = Instant::now() + SURFACE_WAIT;
    let surface = loop {
        if let Some(id) = state.with_mut(|s| surface_in(s, &name)) {
            break Some(id);
        }
        if Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    json(&serde_json::json!({ "session": name, "surface": surface }))
}

fn stop(state: &SharedState, target: &str) -> CommandResult {
    let entry = match checked(target) {
        Ok(entry) => entry,
        Err(refused) => return refused,
    };
    let name = entry.session_name();
    let removed = state.with_mut(|s| {
        let demand = s.mirror_demand_mut();
        if demand.config.iter().any(|e| e.session_name() == name) {
            return Err(refuse(
                ErrorCode::PreconditionFailed,
                "that mirror is a [[mirrors]] entry; remove it from config.toml instead",
            ));
        }
        Ok(demand.started.remove(&name).is_some())
    });
    match removed {
        Err(refused) => refused,
        Ok(false) => refuse(
            ErrorCode::NotFound,
            format!("no mirror of {target:?} was started"),
        ),
        Ok(true) => {
            super::request_reload(state);
            json(&serde_json::json!({ "session": name }))
        }
    }
}

/// The local id of the live surface in session `name`.
fn surface_in(s: &mut ServerState, name: &str) -> Option<u32> {
    let session = s.find_session_by_name(name)?;
    let surface = s
        .registry()
        .session(session)?
        .members
        .iter()
        .find_map(|key| {
            let id = s.registry().resource_id(key)?;
            let record = s.registry().record(id)?;
            (record.kind == phux_core::ResourceKind::PixelSurface
                && s.resource_handle(id).is_some())
            .then_some(id)
        })?;
    match s.intern_resource_wire(surface) {
        phux_protocol::ids::ResourceId::Local { id } => Some(id),
        phux_protocol::ids::ResourceId::Satellite { .. } => None,
    }
}

/// This host's capture targets, from `phux agent surface targets --json`.
async fn list_here(state: &SharedState) -> Result<serde_json::Value, String> {
    let exe = state
        .with(ServerState::native_host_executable)
        .map_err(|err| err.to_string())?;
    let run = tokio::process::Command::new(exe)
        .args(["agent", "surface", "targets", "--json"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(LIST_TIMEOUT, run)
        .await
        .map_err(|_| "listing the capture targets took too long".to_owned())?
        .map_err(|err| format!("could not list the capture targets: {err}"))?;
    parse_targets(&output.stdout)
}

fn parse_targets(stdout: &[u8]) -> Result<serde_json::Value, String> {
    let mut doc: serde_json::Value = serde_json::from_slice(stdout)
        .map_err(|_| "this host's phux could not list capture targets".to_owned())?;
    match doc.get_mut("targets").map(serde_json::Value::take) {
        Some(targets @ serde_json::Value::Array(_)) => Ok(targets),
        _ => Err(doc["error"]["message"]
            .as_str()
            .unwrap_or("this host's phux could not list capture targets")
            .to_owned()),
    }
}

#[cfg(test)]
mod tests;
