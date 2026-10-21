//! Project bound agent state into task state while the keeper is up.
//!
//! The keeper is the only writer. This thread watches the phux server and
//! sends each transition back through [`phux_work::Client`]. Level state is
//! read on start and after every journal or source gap; the journal is a
//! wake-up, not the history of the task.
//!
//! The projector thread is supervised. If it stops or panics, that is logged
//! and the thread is restarted with backoff. On exit, a short grace lets a
//! late `done` record land; that terminal's agent record is re-read before
//! a failure is written.
//!
//! `PHUX_MISSION_PROJECTOR_DELAY_MS` and `PHUX_MISSION_SUBSCRIBE_AFTER` are
//! debug-build test hooks. A release binary does not read them: a stale
//! subscription cursor drops live agent events and projection stalls.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use phux_client::agent_meta::AgentMetaState;
use phux_client::watch::{FleetSubscription, WatchItem, subscribe_fleet_after};
use phux_mission::{
    AgentReport, Projection, ProjectionInput, RunPhase, TerminalFact, observation_operation,
    project,
};
use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::frame::{AgentEvent, ResourceLifecycle};
use phux_protocol::wire::info::ResourceInfo;
use phux_work::{Client, ObjectiveState, OpenRun, StateEdit};

const ACTOR: &str = "keeper";

/// How long an exit waits for a `done` record before failure.
const EXIT_GRACE: Duration = Duration::from_millis(2_500);

/// Bound-id refresh throttle for fleet events that are not exits.
const BOUND_REFRESH: Duration = Duration::from_millis(200);

/// Level-read / open-run attempts before reconcile gives up for this pass.
const RECONCILE_ATTEMPTS: u32 = 4;

/// Watch `server_socket` until the process exits. The socket is bound before
/// this runs, and this thread does not open the store. A projector that
/// stops or panics is logged and restarted with backoff.
pub(crate) fn spawn(server_socket: Option<PathBuf>) {
    let socket = server_socket.unwrap_or_else(default_server_socket);
    let _ = std::thread::Builder::new()
        .name("mission-project".to_owned())
        .spawn(move || supervise(&socket));
}

fn supervise(socket: &Path) {
    let mut backoff = Duration::from_millis(200);
    loop {
        let started = Instant::now();
        let drive = std::thread::Builder::new()
            .name("mission-project-drive".to_owned())
            .spawn({
                let socket = socket.to_path_buf();
                move || drive(socket)
            });
        let handle = match drive {
            Ok(handle) => handle,
            Err(err) => {
                error!("mission projector failed to start: {err}");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
                continue;
            }
        };
        match handle.join() {
            Ok(()) => error!("mission projector stopped; restarting"),
            Err(_) => error!("mission projector panicked; restarting"),
        }
        if started.elapsed() > Duration::from_secs(5) {
            backoff = Duration::from_millis(200);
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
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

struct ProjectionWatch {
    agents: HashMap<String, AgentReport>,
    bound: HashSet<String>,
    missed: HashSet<String>,
    refreshed_at: Option<Instant>,
}

impl ProjectionWatch {
    fn new() -> Self {
        Self {
            agents: HashMap::new(),
            bound: HashSet::new(),
            missed: HashSet::new(),
            refreshed_at: None,
        }
    }

    fn note_runs(&mut self, runs: &[OpenRun]) {
        self.bound.clear();
        for run in runs {
            if let Some(id) = &run.terminal_id {
                self.bound.insert(id.clone());
                self.missed.remove(id);
            }
        }
        self.agents.retain(|id, _| self.bound.contains(id));
        self.refreshed_at = Some(Instant::now());
    }

    fn invalidate(&mut self) {
        self.missed.clear();
        self.refreshed_at = None;
    }

    fn forget(&mut self, id: &str) {
        self.bound.remove(id);
        self.agents.remove(id);
        self.missed.remove(id);
    }

    /// Whether `id` is a bound terminal. Exits pass `force` so a stale miss
    /// cannot drop the one event that ends the run.
    fn tracks(&mut self, client: &Client, id: &str, force: bool) -> bool {
        if self.bound.contains(id) {
            return true;
        }
        let recent = self
            .refreshed_at
            .is_some_and(|at| at.elapsed() < BOUND_REFRESH);
        if !force && recent && self.missed.contains(id) {
            return false;
        }
        self.refreshed_at = Some(Instant::now());
        match client.open_runs() {
            Ok(runs) => self.note_runs(&runs),
            Err(err) => {
                error!("mission projector could not list open runs: {err}");
                return false;
            }
        }
        if self.bound.contains(id) {
            return true;
        }
        self.missed.insert(id.to_owned());
        false
    }
}

async fn watch_server(socket: PathBuf) {
    if let Some(delay) = delay_ms() {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let client = Client::new(super::mission::endpoint(), None);
    let mut cursor = subscribe_after();
    let mut watch = ProjectionWatch::new();
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
        reconcile(&client, &socket, &mut watch).await;
        if !follow(&client, &socket, &mut subscription, &mut watch, live).await {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

enum WatchStep {
    Stay,
    Resubscribe,
    Ended,
}

/// `false` when the stream ended. `true` when a gap asked for a fresh subscribe.
async fn follow(
    client: &Client,
    socket: &Path,
    subscription: &mut FleetSubscription,
    watch: &mut ProjectionWatch,
    live: bool,
) -> bool {
    let mut pending = VecDeque::new();
    loop {
        let next = if let Some(item) = pending.pop_front() {
            Ok(Some(item))
        } else {
            subscription.recv_item().await
        };
        match next {
            Ok(Some(item)) => {
                if is_spawn(&item) {
                    watch.invalidate();
                }
                if is_gap(&item) {
                    note_gap();
                    reconcile(client, socket, watch).await;
                    return true;
                }
                if !live {
                    continue;
                }
                if let Some(exit) = exit_of(&item) {
                    match hold_exit(client, socket, subscription, watch, exit, &mut pending).await {
                        WatchStep::Stay => {}
                        WatchStep::Resubscribe => return true,
                        WatchStep::Ended => return false,
                    }
                } else {
                    apply_live(client, socket, watch, &item);
                }
            }
            Ok(None) => return false,
            Err(err) => {
                error!("mission projector watch: {err}");
                return false;
            }
        }
    }
}

/// Wait out [`EXIT_GRACE`], then project the exit from the re-read record.
async fn hold_exit(
    client: &Client,
    socket: &Path,
    subscription: &mut FleetSubscription,
    watch: &mut ProjectionWatch,
    exit: ExitFact,
    pending: &mut VecDeque<WatchItem>,
) -> WatchStep {
    let Some(id) = terminal_key(&exit.terminal) else {
        return WatchStep::Stay;
    };
    if !watch.tracks(client, &id, true) {
        return WatchStep::Stay;
    }
    info!("keeper holding an exit for a late done record");
    let deadline = tokio::time::Instant::now() + EXIT_GRACE;
    let mut saw_done = watch.agents.get(&id).copied() == Some(AgentReport::Done);
    let mut step = WatchStep::Stay;
    loop {
        let rest = deadline.saturating_duration_since(tokio::time::Instant::now());
        if rest.is_zero() {
            break;
        }
        match tokio::time::timeout(rest, subscription.recv_item()).await {
            Err(_elapsed) => break,
            Ok(Ok(None)) => {
                step = WatchStep::Ended;
                break;
            }
            Ok(Err(err)) => {
                error!("mission projector watch: {err}");
                step = WatchStep::Ended;
                break;
            }
            Ok(Ok(Some(item))) => {
                if is_gap(&item) {
                    note_gap();
                    step = WatchStep::Resubscribe;
                    break;
                }
                if is_spawn(&item) {
                    watch.invalidate();
                }
                if let Some(other) = exit_of(&item) {
                    let same = terminal_key(&other.terminal).as_deref() == Some(id.as_str());
                    if !same {
                        pending.push_back(item);
                    }
                    continue;
                }
                if agent_is_done(&item, &id) {
                    saw_done = true;
                }
                apply_live(client, socket, watch, &item);
            }
        }
    }
    match phux_client::agent_wait::fetch_agent_record(socket, &exit.terminal).await {
        Ok(Some(record)) if report_of(record.state) == AgentReport::Done => {
            saw_done = true;
            info!("keeper read a late done record");
        }
        Ok(_) => {}
        Err(err) => error!("mission projector agent record: {err}"),
    }
    if saw_done {
        watch.agents.insert(id.clone(), AgentReport::Done);
    }
    commit_exit(client, socket, watch, &id, exit.status, exit.seq);
    if matches!(step, WatchStep::Resubscribe) {
        reconcile(client, socket, watch).await;
    }
    step
}

fn commit_exit(
    client: &Client,
    socket: &Path,
    watch: &mut ProjectionWatch,
    id: &str,
    status: Option<i32>,
    seq: u64,
) {
    let Ok(runs) = client.open_runs() else {
        error!("mission projector could not list open runs");
        return;
    };
    watch.note_runs(&runs);
    let Some(run) = runs
        .iter()
        .find(|run| run.terminal_id.as_deref() == Some(id))
    else {
        watch.forget(id);
        return;
    };
    let observed = ProjectionInput {
        current: run.state,
        run: phase(run),
        agent: watch.agents.get(id).copied(),
        terminal: Some(TerminalFact::Exited { status }),
        binding_present: true,
        credential_lost: run.credential_lost,
        launching: false,
    };
    if let Err(err) = write_projection(client, socket, watch, run, &project(&observed), seq) {
        error!("mission projector state write: {err}");
    }
}

fn apply_live(client: &Client, socket: &Path, watch: &mut ProjectionWatch, item: &WatchItem) {
    let WatchItem::AgentState(update) = item else {
        return;
    };
    let Some(terminal) = update.terminal.as_ref() else {
        return;
    };
    let Some(id) = terminal_key(terminal) else {
        return;
    };
    if !watch.tracks(client, &id, false) {
        return;
    }
    let Some(report) = update.record.as_ref().map(|record| report_of(record.state)) else {
        return;
    };
    watch.agents.insert(id.clone(), report);
    let Ok(runs) = client.open_runs() else {
        error!("mission projector could not list open runs");
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
        agent: Some(report),
        terminal: Some(TerminalFact::Running),
        binding_present: true,
        credential_lost: run.credential_lost,
        launching: false,
    };
    if let Err(err) = write_projection(client, socket, watch, run, &project(&observed), 0) {
        error!("mission projector state write: {err}");
    }
    drain_queue(client, socket, watch);
}

async fn reconcile(client: &Client, socket: &Path, watch: &mut ProjectionWatch) {
    let mut delay = Duration::from_millis(50);
    for attempt in 1..=RECONCILE_ATTEMPTS {
        match reconcile_once(client, socket, watch).await {
            Ok(()) => return,
            Err(err) => {
                error!("mission projector reconcile: {err}");
                if attempt == RECONCILE_ATTEMPTS {
                    return;
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(1));
            }
        }
    }
}

async fn reconcile_once(
    client: &Client,
    socket: &Path,
    watch: &mut ProjectionWatch,
) -> Result<(), String> {
    let view = phux_client::state::get_state(socket)
        .await
        .map_err(|err| format!("level state: {err}"))?;
    let complete = view.is_complete();
    let snapshot = view.snapshot().clone();
    let mut notices = Vec::new();
    let records = phux_client::agent_record::fetch_index(socket, &snapshot, &mut notices).await;
    let runs = client
        .open_runs()
        .map_err(|err| format!("open runs: {err}"))?;
    watch.note_runs(&runs);
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
            watch
                .agents
                .insert(bound.to_owned(), report_of(record.state));
        }
        let observed = ProjectionInput {
            current: run.state,
            run: phase(run),
            agent: watch.agents.get(bound).copied(),
            terminal: Some(terminal),
            binding_present: true,
            credential_lost: run.credential_lost,
            launching: false,
        };
        if let Err(err) = write_projection(client, socket, watch, run, &project(&observed), 0) {
            error!("mission projector state write: {err}");
        }
    }
    drain_queue(client, socket, watch);
    Ok(())
}

fn write_projection(
    client: &Client,
    socket: &Path,
    watch: &mut ProjectionWatch,
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
        if let Some(id) = &run.terminal_id {
            watch.forget(id);
        }
        drain_queue(client, socket, watch);
    }
    Ok(())
}

fn drain_queue(client: &Client, socket: &Path, watch: &mut ProjectionWatch) {
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
        Ok(Ok(())) => watch.invalidate(),
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

struct ExitFact {
    terminal: ResourceId,
    status: Option<i32>,
    seq: u64,
}

fn exit_of(item: &WatchItem) -> Option<ExitFact> {
    let WatchItem::Event(event) = item else {
        return None;
    };
    let exit = seen_exit(&event.event)?;
    Some(ExitFact {
        terminal: event.terminal.clone()?,
        status: exit.status,
        seq: event.stamp.as_ref().map_or(0, |stamp| stamp.seq),
    })
}

const fn is_gap(item: &WatchItem) -> bool {
    let WatchItem::Event(event) = item else {
        return false;
    };
    matches!(
        event.event,
        AgentEvent::JournalGap { .. } | AgentEvent::SourceGap { .. }
    )
}

const fn is_spawn(item: &WatchItem) -> bool {
    let WatchItem::Event(event) = item else {
        return false;
    };
    matches!(
        event.event,
        AgentEvent::ResourceSpawned {
            kind: ResourceKind::Terminal,
            ..
        }
    )
}

fn agent_is_done(item: &WatchItem, id: &str) -> bool {
    let WatchItem::AgentState(update) = item else {
        return false;
    };
    let Some(terminal) = update.terminal.as_ref() else {
        return false;
    };
    if terminal_key(terminal).as_deref() != Some(id) {
        return false;
    }
    update
        .record
        .as_ref()
        .is_some_and(|record| report_of(record.state) == AgentReport::Done)
}

fn note_gap() {
    info!("keeper reconciled level state after journal_gap");
}

fn terminal_key(id: &ResourceId) -> Option<String> {
    id.local_id().map(|local| local.to_string())
}

/// Test hook. Release builds do not read `PHUX_MISSION_PROJECTOR_DELAY_MS`.
#[cfg(debug_assertions)]
fn delay_ms() -> Option<u64> {
    std::env::var("PHUX_MISSION_PROJECTOR_DELAY_MS")
        .ok()
        .and_then(|text| text.parse().ok())
        .filter(|delay| *delay > 0)
}

/// Release builds never consult the delay hook.
#[cfg(not(debug_assertions))]
const fn delay_ms() -> Option<u64> {
    None
}

/// Test hook. Release builds do not read `PHUX_MISSION_SUBSCRIBE_AFTER`.
#[cfg(debug_assertions)]
fn subscribe_after() -> Option<u64> {
    std::env::var("PHUX_MISSION_SUBSCRIBE_AFTER")
        .ok()
        .and_then(|text| text.parse().ok())
}

/// Release builds always subscribe live.
#[cfg(not(debug_assertions))]
const fn subscribe_after() -> Option<u64> {
    None
}
