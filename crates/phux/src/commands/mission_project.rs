//! Project bound agent state into task state while the keeper is up.
//!
//! The keeper is the only writer. This thread watches the phux server and
//! sends each transition back through [`phux_work::Client`]. Level state is
//! read on start and after every journal or source gap; the journal is a
//! wake-up, not the history of the task.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_client::agent_meta::AgentMetaState;
use phux_client::watch::{FleetSubscription, WatchItem, subscribe_fleet_after};
use phux_mission::{
    AgentReport, Projection, ProjectionInput, RunPhase, TerminalFact, observation_operation,
    project,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, ResourceLifecycle};
use phux_protocol::wire::info::ResourceInfo;
use phux_work::{Client, ObjectiveState, OpenRun, StateEdit};

const ACTOR: &str = "keeper";

/// Watch `server_socket` until the process exits. The socket is bound before
/// this runs, and this thread does not open the store.
pub(crate) fn spawn(server_socket: Option<PathBuf>) {
    let socket = server_socket.unwrap_or_else(default_server_socket);
    let _ = std::thread::Builder::new()
        .name("mission-project".to_owned())
        .spawn(move || drive(socket));
}

fn drive(socket: PathBuf) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!("mission projector runtime: {err}");
            return;
        }
    };
    runtime.block_on(watch_server(socket));
}

fn default_server_socket() -> PathBuf {
    std::env::var_os("PHUX_SOCKET")
        .map_or_else(phux_server::runtime::default_socket_path, PathBuf::from)
}

async fn watch_server(socket: PathBuf) {
    if let Some(delay) = delay_ms() {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let client = Client::new(super::mission::endpoint(), None);
    let mut cursor = subscribe_after();
    let mut agents = HashMap::<String, AgentReport>::new();
    loop {
        let mut subscription = match subscribe_fleet_after(&socket, cursor).await {
            Ok(subscription) => subscription,
            Err(err) => {
                error!("mission projector subscribe: {err}");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        let live = cursor.is_none();
        cursor = None;
        reconcile(&client, &socket, &mut agents).await;
        if !follow(&client, &socket, &mut subscription, &mut agents, live).await {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// `false` when the stream ended. `true` when a gap asked for a fresh subscribe.
async fn follow(
    client: &Client,
    socket: &Path,
    subscription: &mut FleetSubscription,
    agents: &mut HashMap<String, AgentReport>,
    live: bool,
) -> bool {
    loop {
        match subscription.recv_item().await {
            Ok(Some(WatchItem::Event(event)))
                if matches!(
                    event.event,
                    AgentEvent::JournalGap { .. } | AgentEvent::SourceGap { .. }
                ) =>
            {
                info!("keeper reconciled level state after journal_gap");
                reconcile(client, socket, agents).await;
                return true;
            }
            Ok(Some(item)) if live => apply_live(client, socket, agents, item),
            Ok(Some(_)) => {}
            Ok(None) => return false,
            Err(err) => {
                error!("mission projector watch: {err}");
                return false;
            }
        }
    }
}

fn apply_live(
    client: &Client,
    socket: &Path,
    agents: &mut HashMap<String, AgentReport>,
    item: WatchItem,
) {
    let Ok(runs) = client.open_runs() else {
        error!("mission projector could not list open runs");
        return;
    };
    match item {
        WatchItem::AgentState(update) => {
            let Some(terminal) = update.terminal.as_ref() else {
                return;
            };
            let Some(id) = terminal_key(terminal) else {
                return;
            };
            let Some(run) = runs
                .iter()
                .find(|run| run.terminal_id.as_deref() == Some(id.as_str()))
            else {
                return;
            };
            let Some(report) = update.record.as_ref().map(|record| report_of(record.state)) else {
                return;
            };
            agents.insert(id, report);
            let observed = ProjectionInput {
                current: run.state,
                run: phase(run),
                agent: Some(report),
                terminal: Some(TerminalFact::Running),
                binding_present: true,
                credential_lost: run.credential_lost,
                launching: false,
            };
            if let Err(err) = write_projection(client, socket, run, &project(&observed), 0) {
                error!("mission projector state write: {err}");
            }
        }
        WatchItem::Event(event) => {
            let Some(terminal) = event.terminal.as_ref() else {
                return;
            };
            let Some(id) = terminal_key(terminal) else {
                return;
            };
            let Some(exit) = seen_exit(&event.event) else {
                return;
            };
            let Some(run) = runs
                .iter()
                .find(|run| run.terminal_id.as_deref() == Some(id.as_str()))
            else {
                return;
            };
            let observed = ProjectionInput {
                current: run.state,
                run: phase(run),
                agent: agents.get(&id).copied(),
                terminal: Some(TerminalFact::Exited {
                    status: exit.status,
                }),
                binding_present: true,
                credential_lost: run.credential_lost,
                launching: false,
            };
            let seq = event.stamp.as_ref().map_or(0, |stamp| stamp.seq);
            if let Err(err) = write_projection(client, socket, run, &project(&observed), seq) {
                error!("mission projector state write: {err}");
            }
        }
    }
    drain_queue(client, socket);
}

async fn reconcile(client: &Client, socket: &Path, agents: &mut HashMap<String, AgentReport>) {
    let Ok(view) = phux_client::state::get_state(socket).await else {
        error!("mission projector could not read level state");
        return;
    };
    let complete = view.is_complete();
    let snapshot = view.snapshot().clone();
    let mut notices = Vec::new();
    let records = phux_client::agent_record::fetch_index(socket, &snapshot, &mut notices).await;
    let Ok(runs) = client.open_runs() else {
        error!("mission projector could not list open runs");
        return;
    };
    for run in &runs {
        let Some(bound) = run.terminal_id.as_deref() else {
            continue;
        };
        let resource = snapshot
            .resources
            .iter()
            .find(|resource| terminal_key(&resource.id).as_deref() == Some(bound));
        let terminal = match resource {
            Some(resource) => fact_of(resource),
            None if complete => TerminalFact::Gone,
            None => continue,
        };
        if let Some(resource) = resource
            && let Some(record) = records.get(&resource.id)
        {
            agents.insert(bound.to_owned(), report_of(record.state));
        }
        let observed = ProjectionInput {
            current: run.state,
            run: phase(run),
            agent: agents.get(bound).copied(),
            terminal: Some(terminal),
            binding_present: true,
            credential_lost: run.credential_lost,
            launching: false,
        };
        if let Err(err) = write_projection(client, socket, run, &project(&observed), 0) {
            error!("mission projector state write: {err}");
        }
    }
    drain_queue(client, socket);
}

fn write_projection(
    client: &Client,
    socket: &Path,
    run: &OpenRun,
    projected: &Projection,
    seq: u64,
) -> Result<(), phux_work::Error> {
    let Some(kind) = projected.event_kind else {
        return Ok(());
    };
    let operation_id = observation_operation(
        &run.objective_id,
        &run.run_id,
        run.state,
        run.revision,
        kind,
        seq,
    );
    let applied = client.record_state(StateEdit {
        alias: run.alias.clone(),
        state: projected.next,
        expected_revision: run.revision,
        operation_id,
        actor: ACTOR.to_owned(),
        event_kind: Some(kind.to_owned()),
    })?;
    if applied.objective.state.frees_slot() {
        drain_queue(client, socket);
    }
    Ok(())
}

fn drain_queue(client: &Client, socket: &Path) {
    let Ok(runs) = client.open_runs() else {
        return;
    };
    let Some(queued) = runs.iter().find(|run| run.run_state == "queued") else {
        return;
    };
    let running = runs.iter().filter(|run| run.run_state == "running").count();
    let max_agents = super::mission_start::configured_max_agents(Path::new(&queued.repo_root))
        .unwrap_or(queued.max_agents);
    if max_agents == 0 || running >= usize::try_from(max_agents).unwrap_or(usize::MAX) {
        return;
    }
    let operation_id = observation_operation(
        &queued.objective_id,
        &queued.run_id,
        ObjectiveState::Queued,
        0,
        "launch",
        0,
    );
    match client.replay_launch(operation_id) {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(err) => {
            error!("mission queue replay: {err}");
            return;
        }
    }
    // `promote_queued` builds a worktree session with its own runtime. This
    // thread is already driving one, so the launch has to hop off it.
    let client = client.clone();
    let queued = queued.clone();
    let socket = socket.to_path_buf();
    let launched = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                super::mission_start::promote_queued(
                    &client,
                    &queued,
                    max_agents,
                    operation_id,
                    &socket,
                )
            })
            .join()
    });
    match launched {
        Ok(Ok(())) => {}
        Ok(Err(err)) => error!("mission queue: {err}"),
        Err(_) => error!("mission queue launch panicked"),
    }
}

fn phase(run: &OpenRun) -> RunPhase {
    if run.run_state == "queued" {
        RunPhase::Queued
    } else {
        RunPhase::Running
    }
}

const fn report_of(state: AgentMetaState) -> AgentReport {
    match state {
        AgentMetaState::Working => AgentReport::Working,
        AgentMetaState::Blocked => AgentReport::Blocked,
        AgentMetaState::Done => AgentReport::Done,
        AgentMetaState::Idle | AgentMetaState::Unknown => AgentReport::Other,
    }
}

fn fact_of(resource: &ResourceInfo) -> TerminalFact {
    if resource.lifecycle == ResourceLifecycle::Exited {
        TerminalFact::Exited {
            status: resource.exit.as_ref().and_then(|exit| exit.exit_status),
        }
    } else {
        TerminalFact::Running
    }
}

struct SeenExit {
    status: Option<i32>,
}

const fn seen_exit(event: &AgentEvent) -> Option<SeenExit> {
    match event {
        AgentEvent::TerminalControl {
            lifecycle: ResourceLifecycle::Exited,
            exit_status,
            ..
        }
        | AgentEvent::ResourceClosed { exit_status } => Some(SeenExit {
            status: *exit_status,
        }),
        _ => None,
    }
}

fn terminal_key(id: &ResourceId) -> Option<String> {
    id.local_id().map(|local| local.to_string())
}

fn delay_ms() -> Option<u64> {
    std::env::var("PHUX_MISSION_PROJECTOR_DELAY_MS")
        .ok()
        .and_then(|text| text.parse().ok())
        .filter(|delay| *delay > 0)
}

fn subscribe_after() -> Option<u64> {
    std::env::var("PHUX_MISSION_SUBSCRIBE_AFTER")
        .ok()
        .and_then(|text| text.parse().ok())
}
