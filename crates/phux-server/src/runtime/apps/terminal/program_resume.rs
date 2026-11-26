//! OSC 7577 records on a pane's launch spec, and the global index `phux ls -l`
//! and `phux restore --dry-run` read. The per-pane token never enters either.

use phux_core::{LaunchSpec, ProgramResume, ProgramResumeUpdate};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{PROGRAM_RESUME_INDEX_KEY, Scope};
use tokio::sync::mpsc;

use crate::state::{ServerState, SharedState};

/// How long an owner may look exited before the record is dropped, so a
/// process that dies with the terminal can still be resumed.
const OWNER_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Apply each report, and drop a record whose owner has been gone for [`OWNER_GRACE`].
pub(in crate::runtime) fn spawn_drain(
    state: SharedState,
    terminal: ResourceId,
    mut rx: mpsc::UnboundedReceiver<ProgramResumeUpdate>,
) {
    tokio::task::spawn_local(async move {
        let mut gone_since = None;
        loop {
            tokio::select! {
                update = rx.recv() => {
                    let Some(update) = update else { break };
                    state.with_mut(|server| apply(server, &terminal, update));
                }
                () = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                    state.with_mut(|server| {
                        watch_owner(server, &terminal, &mut gone_since);
                    });
                }
            }
        }
    });
}

fn apply(server: &mut ServerState, terminal: &ResourceId, update: ProgramResumeUpdate) {
    if server.shutting_down() || !server.config().restore.program_resume {
        return;
    }
    let changed = match update {
        ProgramResumeUpdate::Put(record) => put(server, terminal, record),
        ProgramResumeUpdate::Clear { pid } => clear(server, terminal, pid),
    };
    if changed {
        publish_program_resumes(server);
    }
}

fn put(server: &mut ServerState, terminal: &ResourceId, record: ProgramResume) -> bool {
    if !record.is_valid() {
        return false;
    }
    let stale = current(server, terminal)
        .is_some_and(|stored| stored.pid == record.pid && record.time < stored.time);
    if stale {
        return false;
    }
    super::resume::update_launch(server, terminal, |launch| {
        launch.program_resume = Some(Box::new(record));
    })
    .is_ok()
}

fn clear(server: &mut ServerState, terminal: &ResourceId, pid: u32) -> bool {
    let matches = current(server, terminal).is_some_and(|stored| stored.pid == pid);
    if !matches {
        return false;
    }
    super::resume::update_launch(server, terminal, |launch| launch.program_resume = None).is_ok()
}

fn current(server: &ServerState, terminal: &ResourceId) -> Option<ProgramResume> {
    let id = server.idspace.resource_from_wire(terminal)?;
    let record = server.registry().record(id)?;
    match record.launch.as_ref() {
        Some(LaunchSpec::Terminal(launch)) => launch.program_resume.as_deref().cloned(),
        _ => None,
    }
}

fn watch_owner(
    server: &mut ServerState,
    terminal: &ResourceId,
    gone_since: &mut Option<std::time::Instant>,
) {
    let Some(record) = current(server, terminal) else {
        *gone_since = None;
        return;
    };
    if !owner_gone(&record) {
        *gone_since = None;
        return;
    }
    let since = gone_since.get_or_insert_with(std::time::Instant::now);
    if since.elapsed() >= OWNER_GRACE && clear(server, terminal, record.pid) {
        *gone_since = None;
        publish_program_resumes(server);
    }
}

fn owner_gone(record: &ProgramResume) -> bool {
    let Ok(pid) = i32::try_from(record.pid) else {
        return true;
    };
    phux_resource::proc_query::process_start_time(pid)
        .is_none_or(|started| record.started != 0 && started != record.started)
}

/// Republish every pane's escaped resume command. The token is not in the record.
pub(in crate::runtime) fn publish_program_resumes(server: &mut ServerState) {
    let mut index = serde_json::Map::new();
    for resource in server.org().resources() {
        let Some(LaunchSpec::Terminal(launch)) = resource.launch.as_ref() else {
            continue;
        };
        let Some(record) = launch
            .program_resume
            .as_ref()
            .filter(|record| record.is_valid())
        else {
            continue;
        };
        index.insert(
            resource.key.to_string(),
            serde_json::json!({
                "command": record.escaped_command(),
                "trusted": record.trusted,
                "tried": record.tried,
            }),
        );
    }
    let bytes = serde_json::to_vec(&index).unwrap_or_else(|_| b"{}".to_vec());
    server.metadata_set(&Scope::Global, PROGRAM_RESUME_INDEX_KEY, bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_core::{Actor, ExitPolicy, TerminalLaunch};

    fn record(trusted: bool) -> ProgramResume {
        ProgramResume {
            argv: vec!["nvim".into(), "-S".into(), "/tmp/session.vim".into()],
            cwd: Some("/tmp".into()),
            pid: 7,
            started: 1,
            uid: rustix::process::geteuid().as_raw(),
            time: 1_795_543_200,
            tried: false,
            trusted,
        }
    }

    fn saved(state: &SharedState, pane: phux_core::ids::ResourceId) -> Option<ProgramResume> {
        state.with(|server| {
            match server
                .registry()
                .record(pane)
                .and_then(|record| record.launch.clone())
            {
                Some(LaunchSpec::Terminal(launch)) => launch.program_resume.as_deref().cloned(),
                _ => None,
            }
        })
    }

    #[test]
    fn a_report_is_stored_and_config_off_stores_nothing_and_the_token_is_absent() {
        let state = SharedState::new();
        let pane = state.with_mut(|server| {
            server.config_mut().restore.program_resume = true;
            let (_, id) = server
                .create_terminal(
                    crate::state::TerminalTarget::NewSession {
                        name: "resume",
                        lifetime: phux_core::SessionLifetime::Persistent,
                    },
                    TerminalLaunch::default(),
                    ExitPolicy::Retain,
                    &Actor::owner(),
                )
                .expect("create");
            id
        });
        let wire = state.with_mut(|server| server.idspace.intern_resource(pane));
        let secret = "token-must-not-leak";
        state.with_mut(|server| {
            apply(server, &wire, ProgramResumeUpdate::Put(record(true)));
        });
        let stored = saved(&state, pane).expect("record");
        assert!(stored.trusted);
        assert!(!format!("{stored:?}").contains(secret));
        state.with(|server| {
            let value = server
                .metadata()
                .get(&Scope::Global, PROGRAM_RESUME_INDEX_KEY)
                .unwrap_or_default();
            let text = String::from_utf8_lossy(&value);
            assert!(text.contains("nvim"));
            assert!(!text.contains(secret));
            let launch = serde_json::to_string(&stored).expect("json");
            assert!(!launch.contains(secret));
        });
        state.with_mut(|server| {
            server.config_mut().restore.program_resume = false;
            apply(server, &wire, ProgramResumeUpdate::Put(record(false)));
        });
        assert!(saved(&state, pane).expect("unchanged").trusted);
    }

    #[test]
    fn an_older_time_is_ignored_and_clear_removes_only_that_owner() {
        let state = SharedState::new();
        let (pane, wire) = state.with_mut(|server| {
            server.config_mut().restore.program_resume = true;
            let (_, id) = server
                .create_terminal(
                    crate::state::TerminalTarget::NewSession {
                        name: "clock",
                        lifetime: phux_core::SessionLifetime::Persistent,
                    },
                    TerminalLaunch::default(),
                    ExitPolicy::Retain,
                    &Actor::owner(),
                )
                .expect("create");
            let wire = server.idspace.intern_resource(id);
            (id, wire)
        });
        state.with_mut(|server| apply(server, &wire, ProgramResumeUpdate::Put(record(true))));
        let mut older = record(true);
        older.time -= 1;
        state.with_mut(|server| apply(server, &wire, ProgramResumeUpdate::Put(older)));
        assert_eq!(saved(&state, pane).unwrap().time, record(true).time);
        state.with_mut(|server| apply(server, &wire, ProgramResumeUpdate::Clear { pid: 8 }));
        assert!(saved(&state, pane).is_some());
        state.with_mut(|server| apply(server, &wire, ProgramResumeUpdate::Clear { pid: 7 }));
        assert!(saved(&state, pane).is_none());
    }
}
