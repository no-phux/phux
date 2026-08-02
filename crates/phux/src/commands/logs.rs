//! `phux logs` — where phux's logs live, and a tail over any of them.
//!
//! phux writes two log families nobody used to be able to discover from
//! the CLI: the canonical server log (one file, every spawn path) and the
//! per-pid client logs. A crash was durable but unreachable — the file
//! existed, and no command would name it. Bare `phux logs` prints the
//! inventory: every path, whether it exists yet, its size and age, and
//! what to run next. `--server` / `--client` tail one of them (`-f`
//! follows, `-n` sizes the tail, `--pid` picks a specific client), and
//! `--json` emits a stable document for machines.
//!
//! The path knowledge deliberately does not live here: the server log
//! resolves through `phux_server::telemetry::server_log_path` and the
//! client naming convention through the same module, so the writers and
//! this reader can never disagree. What does live here is shared reading
//! machinery: [`tail_file`] (which `phux service logs` delegates to) and
//! the `client-<pid>.log` scan (which `phux service prune-logs` borrows).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

/// How many client logs the human inventory lists before summarizing the
/// rest as a count. The newest few are the ones a crash investigation
/// wants; a long-lived host has hundreds of stale ones.
const SHOWN_CLIENT_LOGS: usize = 3;

/// `phux logs [--server | --client [--pid PID]] [-f] [-n NUM] [--json]`.
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "the parameters mirror the clap flags one-to-one, and clap's \
              own groups/conflicts already forbid the contradictory \
              combinations before this is called"
)]
pub(crate) fn run_logs(
    server: bool,
    client: bool,
    pid: Option<u32>,
    follow: bool,
    lines: u32,
    json: bool,
) -> ExitCode {
    if server {
        let log = phux_server::telemetry::server_log_path();
        let missing = format!(
            "phux logs: no server log at {} yet.\n\
             A server writes it when it next starts; run `phux` (auto-spawns) or `phux server`.",
            log.display()
        );
        return tail_file(&log, follow, lines, &missing);
    }

    let dir = phux_server::telemetry::state_dir();
    if client {
        let log = match client_log_target(&dir, pid) {
            Ok(log) => log,
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::FAILURE;
            }
        };
        let missing = format!(
            "phux logs: no client log at {} yet.\n\
             An interactive client (naked `phux`, `phux attach`) writes one per pid;\n\
             bare `phux logs` lists the ones that exist.",
            log.display()
        );
        return tail_file(&log, follow, lines, &missing);
    }

    let inventory = inventory(dir, phux_server::telemetry::server_log_path());
    if json {
        return match serde_json::to_string_pretty(&json_doc(&inventory)) {
            Ok(rendered) => {
                outln!("{rendered}");
                ExitCode::SUCCESS
            }
            // A `--json` path, so the failure is the shared contract line
            // (phux-i0e8.8.3), never prose.
            Err(err) => crate::commands::json_err::emit(
                true,
                &crate::commands::json_err::CliError::new(
                    crate::commands::json_err::codes::JSON_SERIALIZE,
                    format!("could not render the log inventory as JSON: {err}"),
                    "this is a phux bug; run `phux doctor` and report it",
                ),
                1,
            ),
        };
    }
    out!("{}", render_human(&inventory));
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// tailing
// ---------------------------------------------------------------------------

/// Tail `path` with the system `tail`, showing the last `lines` lines and
/// following when asked.
///
/// The one tail implementation: `phux logs --server`, `phux logs --client`,
/// and `phux service logs` all come through here, so a log is always shown
/// the same way. A missing file prints `missing` — a caller-composed,
/// explanatory message, because "which command creates this file" differs
/// per log — and fails.
pub(crate) fn tail_file(path: &Path, follow: bool, lines: u32, missing: &str) -> ExitCode {
    if !path.exists() {
        eprintln!("{missing}");
        return ExitCode::FAILURE;
    }

    let mut command = std::process::Command::new("tail");
    command.arg("-n").arg(lines.to_string());
    if follow {
        command.arg("-f");
    }
    command.arg(path);

    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) | Err(_) => {
            eprintln!("phux logs: could not read {}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// Which client log `--client [--pid PID]` should tail.
///
/// `--pid` is an exact file the caller asked for, so a missing one is left
/// to [`tail_file`]'s missing-file report. Without a pid the newest log is
/// the answer — that is the client most recently alive — and having none at
/// all is its own error, since there is no path to even name.
fn client_log_target(dir: &Path, pid: Option<u32>) -> Result<PathBuf, String> {
    if let Some(pid) = pid {
        return Ok(dir.join(format!("client-{pid}.log")));
    }
    client_logs_newest_first(dir)
        .into_iter()
        .next()
        .ok_or_else(|| {
            format!(
                "phux logs: no client logs in {} yet.\n\
             An interactive client (naked `phux`, `phux attach`) writes one per pid.",
                dir.display()
            )
        })
}

// ---------------------------------------------------------------------------
// the client-log scan (shared with `phux service prune-logs`)
// ---------------------------------------------------------------------------

/// Every `client-*.log` in the state dir, in directory order.
pub(crate) fn client_log_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if is_client_log(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Whether a path is one of the per-pid client logs, by the naming
/// convention `telemetry::default_client_log_path` writes.
fn is_client_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            // Matching exactly the name `telemetry::default_client_log_path`
            // writes — phux's own output, never operator input, so a
            // case-insensitive match would only widen this to files phux
            // did not create.
            name.starts_with("client-")
                && std::path::Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext == "log")
        })
}

/// The client logs sorted newest-modified first (path as the tiebreaker, so
/// the order is deterministic under equal timestamps). A log whose mtime is
/// unreadable sorts last rather than failing the listing.
fn client_logs_newest_first(dir: &Path) -> Vec<PathBuf> {
    let mut with_time: Vec<(SystemTime, PathBuf)> = client_log_paths(dir)
        .unwrap_or_default()
        .into_iter()
        .map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, path)
        })
        .collect();
    with_time.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    with_time.into_iter().map(|(_, path)| path).collect()
}

/// The pid baked into a `client-<pid>.log` name, when it parses as one.
fn client_pid(path: &Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_prefix("client-")?
        .strip_suffix(".log")?
        .parse()
        .ok()
}

// ---------------------------------------------------------------------------
// the inventory
// ---------------------------------------------------------------------------

/// One log file's observable facts. `size: None` means the file does not
/// exist yet — a normal state the inventory reports as such, never an error.
#[derive(Debug)]
struct FileFacts {
    path: PathBuf,
    size: Option<u64>,
    modified: Option<SystemTime>,
}

/// Everything bare `phux logs` reports, gathered once so the human and JSON
/// renderers can never disagree about what was found.
#[derive(Debug)]
struct Inventory {
    state_dir: PathBuf,
    server: FileFacts,
    /// Newest first.
    clients: Vec<FileFacts>,
}

/// Stat one path into its report row; a missing file is a row, not an error.
fn file_facts(path: PathBuf) -> FileFacts {
    match std::fs::metadata(&path) {
        Ok(meta) => FileFacts {
            size: Some(meta.len()),
            modified: meta.modified().ok(),
            path,
        },
        Err(_) => FileFacts {
            path,
            size: None,
            modified: None,
        },
    }
}

/// Gather the inventory. Takes the resolved paths rather than reading the
/// environment so tests can drive it against a temp dir; the real caller
/// resolves both through `phux_server::telemetry`, the same helpers the
/// writers use.
fn inventory(state_dir: PathBuf, server_log: PathBuf) -> Inventory {
    let clients = client_logs_newest_first(&state_dir)
        .into_iter()
        .map(file_facts)
        .collect();
    Inventory {
        server: file_facts(server_log),
        state_dir,
        clients,
    }
}

/// The human inventory: every path with its facts, then what to run next.
fn render_human(inventory: &Inventory) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "Server log (every spawn path writes here):");
    let _ = writeln!(out, "  {}", describe(&inventory.server));
    let _ = writeln!(out);
    let _ = writeln!(out, "Client logs (one per client pid, newest first):");
    if inventory.clients.is_empty() {
        let _ = writeln!(
            out,
            "  none yet (an interactive client writes client-<pid>.log in the state dir)"
        );
    } else {
        for facts in inventory.clients.iter().take(SHOWN_CLIENT_LOGS) {
            let _ = writeln!(out, "  {}", describe(facts));
        }
        let extra = inventory.clients.len().saturating_sub(SHOWN_CLIENT_LOGS);
        if extra > 0 {
            let _ = writeln!(
                out,
                "  ... and {extra} more (`phux service prune-logs` clears them)"
            );
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "State dir: {}", inventory.state_dir.display());
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Tail with `phux logs --server` or `phux logs --client [--pid PID]`;\n\
         `phux doctor` checks the whole install."
    );
    out
}

/// One inventory line: the path, then size and age — or "not created yet",
/// which is a normal state worth saying plainly.
fn describe(facts: &FileFacts) -> String {
    let Some(size) = facts.size else {
        return format!("{} (not created yet)", facts.path.display());
    };
    let age = facts
        .modified
        .and_then(|modified| modified.elapsed().ok())
        .map_or_else(String::new, |elapsed| {
            format!(", written {} ago", human_age(elapsed))
        });
    format!("{} ({}{age})", facts.path.display(), human_size(size))
}

/// Bytes as a short human figure. Integer math: one decimal of KiB/MiB is
/// as precise as a log size needs to be.
fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{}.{} KiB", bytes / KIB, (bytes % KIB) * 10 / KIB)
    } else {
        format!("{}.{} MiB", bytes / MIB, (bytes % MIB) * 10 / MIB)
    }
}

/// An elapsed duration as the coarsest unit that still reads as an age.
fn human_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 60 * 60 {
        format!("{}m", secs / 60)
    } else if secs < 24 * 60 * 60 {
        format!("{}h", secs / (60 * 60))
    } else {
        format!("{}d", secs / (24 * 60 * 60))
    }
}

/// The `--json` document. `schema_version` 1; additive changes only within
/// a version, like every other `--json` surface in this binary.
fn json_doc(inventory: &Inventory) -> serde_json::Value {
    let clients: Vec<_> = inventory
        .clients
        .iter()
        .map(|facts| {
            let mut row = file_json(facts);
            if let Some(object) = row.as_object_mut() {
                object.insert("pid".to_owned(), serde_json::json!(client_pid(&facts.path)));
            }
            row
        })
        .collect();
    serde_json::json!({
        "schema_version": 1,
        "state_dir": inventory.state_dir.display().to_string(),
        "server_log": file_json(&inventory.server),
        "client_logs": clients,
    })
}

/// One file's JSON row. `size_bytes` and `modified_unix` are `null` exactly
/// when `exists` is false (or the mtime was unreadable).
fn file_json(facts: &FileFacts) -> serde_json::Value {
    let modified_unix = facts
        .modified
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|since| since.as_secs());
    serde_json::json!({
        "path": facts.path.display().to_string(),
        "exists": facts.size.is_some(),
        "size_bytes": facts.size,
        "modified_unix": modified_unix,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "test code; a failed unwrap is the failure"
    )]

    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use clap::Parser as _;

    use super::{
        client_log_target, client_logs_newest_first, client_pid, inventory, json_doc, render_human,
        tail_file,
    };
    use crate::Cli;

    /// Write a client log and pin its mtime, so "newest" is a controlled
    /// fact rather than a race against the filesystem clock.
    fn write_client_log(dir: &Path, name: &str, age: Duration) {
        let path = dir.join(name);
        std::fs::write(&path, b"log line\n").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    /// The acceptance case for a fresh machine: no server has ever run and
    /// the state dir is empty (or absent). The inventory must still print
    /// every path — as "not created yet" — plus the state dir and the
    /// doctor hint, because the moment of need is exactly when nothing
    /// exists yet.
    #[test]
    fn bare_inventory_reports_missing_files_as_not_created_yet() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().to_path_buf();
        let server_log = state.join("server.log");

        let inv = inventory(state.clone(), server_log.clone());
        let human = render_human(&inv);
        assert!(human.contains(&server_log.display().to_string()));
        assert!(human.contains("not created yet"));
        assert!(human.contains("none yet"));
        assert!(human.contains(&format!("State dir: {}", state.display())));
        assert!(human.contains("phux doctor"), "must point at doctor");

        // A state dir that does not even exist yet is the same story, not
        // an error.
        let absent = state.join("never-created");
        let inv = inventory(absent.clone(), absent.join("server.log"));
        let human = render_human(&inv);
        assert!(human.contains("not created yet"));
        assert!(human.contains("none yet"));
    }

    /// An existing server log reports its facts instead of the missing-file
    /// line, and the client list shows newest first with the overflow
    /// summarized.
    #[test]
    fn inventory_reports_existing_files_with_size_and_age() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server.log"), b"started\n").unwrap();
        for (name, age) in [
            ("client-1.log", 400),
            ("client-2.log", 300),
            ("client-3.log", 200),
            ("client-4.log", 100),
        ] {
            write_client_log(dir.path(), name, Duration::from_secs(age));
        }

        let inv = inventory(dir.path().to_path_buf(), dir.path().join("server.log"));
        assert_eq!(inv.clients.len(), 4);
        assert!(
            inv.clients[0].path.ends_with("client-4.log"),
            "newest first"
        );

        let human = render_human(&inv);
        assert!(!human.contains("not created yet"));
        assert!(human.contains("8 B"));
        assert!(
            human.contains("... and 1 more"),
            "overflow past the shown few is a count, not a wall: {human}"
        );
    }

    /// `--client` picks the newest log by mtime; `--pid` overrides with the
    /// exact per-pid file.
    #[test]
    fn client_selection_prefers_newest_and_honors_pid() {
        let dir = tempfile::tempdir().unwrap();
        write_client_log(dir.path(), "client-100.log", Duration::from_secs(3600));
        write_client_log(dir.path(), "client-200.log", Duration::from_secs(60));
        // Not a client log; must never be selected or listed.
        std::fs::write(dir.path().join("server.log"), b"x").unwrap();

        let newest = client_logs_newest_first(dir.path());
        assert_eq!(newest.len(), 2);
        assert!(newest[0].ends_with("client-200.log"));

        let picked = client_log_target(dir.path(), None).unwrap();
        assert!(picked.ends_with("client-200.log"));

        let picked = client_log_target(dir.path(), Some(100)).unwrap();
        assert!(picked.ends_with("client-100.log"));

        // A pid nobody ran still names the exact path, so the missing-file
        // report can say where the log would have been.
        let picked = client_log_target(dir.path(), Some(31337)).unwrap();
        assert!(picked.ends_with("client-31337.log"));
    }

    /// With no client logs at all, `--client` has no path to even name, so
    /// the selection itself reports — with the dir and the how-one-appears
    /// explanation.
    #[test]
    fn client_selection_with_no_logs_explains_itself() {
        let dir = tempfile::tempdir().unwrap();
        let err = client_log_target(dir.path(), None).unwrap_err();
        assert!(err.contains("no client logs"));
        assert!(err.contains(&dir.path().display().to_string()));
        assert!(err.contains("phux attach"), "must say how a log appears");
    }

    /// The `--json` schema is a contract: `schema_version` 1, with the
    /// pinned keys. A rename or removal must fail here first.
    #[test]
    fn json_inventory_schema_is_pinned_at_version_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server.log"), b"started\n").unwrap();
        write_client_log(dir.path(), "client-42.log", Duration::from_secs(10));

        let doc = json_doc(&inventory(
            dir.path().to_path_buf(),
            dir.path().join("server.log"),
        ));
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["state_dir"], dir.path().display().to_string().as_str());
        assert_eq!(doc["server_log"]["exists"], true);
        assert_eq!(doc["server_log"]["size_bytes"], 8);
        assert!(doc["server_log"]["modified_unix"].is_u64());
        assert_eq!(doc["client_logs"][0]["pid"], 42);
        assert_eq!(doc["client_logs"][0]["exists"], true);

        // A missing server log is exists=false with null facts, never an
        // absent key.
        let doc = json_doc(&inventory(
            dir.path().to_path_buf(),
            dir.path().join("no-such.log"),
        ));
        assert_eq!(doc["server_log"]["exists"], false);
        assert!(doc["server_log"]["size_bytes"].is_null());
        assert!(doc["server_log"]["modified_unix"].is_null());
    }

    /// The pid parse takes exactly the shape the client writer produces.
    #[test]
    fn client_pid_parses_only_the_writers_naming() {
        assert_eq!(client_pid(Path::new("/s/client-4242.log")), Some(4242));
        assert_eq!(client_pid(Path::new("/s/client-x.log")), None);
        assert_eq!(client_pid(Path::new("/s/server.log")), None);
    }

    /// A missing file fails with the caller's explanatory message instead
    /// of running `tail` into an error nobody composed.
    #[test]
    fn tail_of_a_missing_file_fails() {
        let dir = tempfile::tempdir().unwrap();
        let code = tail_file(&dir.path().join("absent.log"), false, 10, "explain");
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", std::process::ExitCode::FAILURE)
        );
    }

    /// The clap surface: `--json` is the inventory for machines, so it
    /// conflicts with the tail flags; `--server` and `--client` are one
    /// choice; the tail modifiers require a tail target.
    #[test]
    fn logs_flag_grammar_rejects_contradictions() {
        assert!(Cli::try_parse_from(["phux", "logs"]).is_ok());
        assert!(Cli::try_parse_from(["phux", "logs", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["phux", "logs", "--server", "-f", "-n", "50"]).is_ok());
        assert!(Cli::try_parse_from(["phux", "logs", "--client", "--pid", "42"]).is_ok());

        assert!(
            Cli::try_parse_from(["phux", "logs", "--json", "-f"]).is_err(),
            "--json is the inventory; it cannot follow"
        );
        assert!(
            Cli::try_parse_from(["phux", "logs", "--server", "--client"]).is_err(),
            "one tail target at a time"
        );
        assert!(
            Cli::try_parse_from(["phux", "logs", "-f"]).is_err(),
            "-f without a tail target has nothing to follow"
        );
        assert!(
            Cli::try_parse_from(["phux", "logs", "--pid", "42"]).is_err(),
            "--pid only selects a client log"
        );
    }
}
