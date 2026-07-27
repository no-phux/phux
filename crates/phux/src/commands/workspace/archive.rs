use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

use crate::commands::new::create_session_via_metadata;
use crate::commands::{cli_runtime, partial, report_no_server};

mod model;
mod snapshot;

use model::{ARCHIVE_SCHEMA_VERSION, RestoreSummary, parse_archive, restore_plan};
use snapshot::archive_from_snapshot;

pub(super) fn run_save(socket: Option<PathBuf>, output: Option<&PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let (snapshot, degradation) = match rt.block_on(phux_client::state::get_state(&socket_path)) {
        Ok(view) => view.into_parts(),
        Err(err) => return report_no_server(&err, &socket_path, "workspace save"),
    };
    // A save writes a file the user will restore *later*, so an incomplete
    // capture is the one degradation that outlives the command: panes on an
    // unreachable satellite are simply not in the archive, and nothing at
    // restore time can tell they were meant to be. Still not a failure —
    // refusing to snapshot this laptop because a remote box is down would be
    // worse — but it has to be said out loud before the file lands.
    partial::warn_partial_view("workspace save", &degradation);
    let archive = archive_from_snapshot(&snapshot);
    let rendered = match serde_json::to_string_pretty(&archive) {
        Ok(rendered) => rendered,
        Err(err) => return fail(&format!("could not render workspace archive: {err}")),
    };
    if let Some(path) = output {
        match fs::write(path, rendered) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(&format!("could not write {}: {err}", path.display())),
        }
    } else {
        outln!("{rendered}");
        ExitCode::SUCCESS
    }
}

pub(super) fn run_restore(archive_path: &Path, socket: Option<PathBuf>) -> ExitCode {
    let input = match read_archive_text(archive_path) {
        Ok(input) => input,
        Err(err) => return fail(&err),
    };
    let archive = match parse_archive(&input) {
        Ok(archive) => archive,
        Err(err) => return fail(&err),
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let existing = match rt.block_on(fetch_existing_sessions(&socket_path)) {
        Ok(existing) => existing,
        Err(code) => return code,
    };
    let plan = match restore_plan(&archive, &existing) {
        Ok(plan) => plan,
        Err(err) => return fail(&err),
    };
    let mut restored = Vec::with_capacity(plan.creates.len());
    for create in plan.creates {
        match rt.block_on(create_session_via_metadata(
            &socket_path,
            &create.name,
            create.command,
            create.cwd,
            BTreeMap::default(),
        )) {
            Ok(_) => restored.push(create.name),
            Err(code) => return code,
        }
    }
    let summary = RestoreSummary {
        schema_version: ARCHIVE_SCHEMA_VERSION,
        restored,
        skipped_existing: plan.skipped_existing,
    };
    match serde_json::to_string_pretty(&summary) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&format!("could not render restore summary: {err}")),
    }
}

fn read_archive_text(path: &Path) -> Result<String, String> {
    if path == Path::new("-") {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|err| format!("could not read workspace archive from stdin: {err}"))?;
        return Ok(input);
    }
    fs::read_to_string(path)
        .map_err(|err| format!("could not read workspace archive {}: {err}", path.display()))
}

/// The session names already on the server, for restore's collision check.
///
/// `into_snapshot_ignoring_degradation`: this reads `sessions` only, and a
/// satellite's session list never enters the merge —
/// `handle_get_state_federated` discards it because its `u32` ids would
/// collide with the hub's. An unreachable satellite therefore cannot hide a
/// name this check would otherwise catch.
async fn fetch_existing_sessions(socket_path: &Path) -> Result<Vec<String>, ExitCode> {
    phux_client::state::get_state(socket_path)
        .await
        .map(|view| {
            view.into_snapshot_ignoring_degradation()
                .sessions
                .into_iter()
                .map(|session| session.name)
                .collect()
        })
        .map_err(|err| report_no_server(&err, socket_path, "workspace restore"))
}

fn fail(message: &str) -> ExitCode {
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}
