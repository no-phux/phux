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

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::watch::{FleetSubscription, WatchItem, subscribe_fleet_after};
use phux_protocol::ids::{AuthorityId, ResourceId};
use phux_work::mission::{
    AgentReport, Projection, ProjectionInput, RunPhase, TerminalFact, observation_operation,
    project,
};
use phux_work::{Client, ObjectiveState, OpenRun, RunState, StateEdit};

const ACTOR: &str = "keeper";

/// How long an exit waits for a `done` record before failure.
const EXIT_GRACE: Duration = Duration::from_millis(2_500);

/// Bound-id refresh throttle for fleet events that are not exits.
const BOUND_REFRESH: Duration = Duration::from_millis(200);

/// Level-read / open-run attempts before reconcile gives up for this pass.
const RECONCILE_ATTEMPTS: u32 = 4;

mod facts;
use facts::{ExitFact, exit_of, is_gap, is_spawn, observe, phase};

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
        .map_or_else(phux_config::socket::default_socket_path, PathBuf::from)
}

struct ProjectionWatch {
    bound: HashSet<String>,
    missed: HashSet<String>,
    refreshed_at: Option<Instant>,
}

impl ProjectionWatch {
    fn new() -> Self {
        Self {
            bound: HashSet::new(),
            missed: HashSet::new(),
            refreshed_at: None,
        }
    }

    fn note_runs(&mut self, runs: &[OpenRun]) {
        self.bound.clear();
        for run in runs {
            if let Some(id) = tracked_id(run) {
                self.bound.insert(id.to_owned());
                self.missed.remove(id);
            }
        }
        self.refreshed_at = Some(Instant::now());
    }

    fn invalidate(&mut self) {
        self.missed.clear();
        self.refreshed_at = None;
    }

    fn forget(&mut self, id: &str) {
        self.bound.remove(id);
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
    let mut level = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    level.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let next = if let Some(item) = pending.pop_front() {
            Ok(Some(item))
        } else {
            tokio::select! {
                result = subscription.recv_item() => result,
                _ = level.tick() => {
                    if let Err(error) = reconcile_once(client, socket, watch).await {
                        error!("mission projector periodic reconcile: {error}");
                    }
                    continue;
                },
            }
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
                    apply_live(client, socket, watch, &item).await;
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
                // Keep this worker's late declaration for the verified level
                // read below; all other workers can still project normally.
                if agent_update_id(&item).as_deref() != Some(id.as_str()) {
                    apply_live(client, socket, watch, &item).await;
                }
            }
        }
    }
    project_hint(client, socket, watch, &id, exit.seq).await;
    if matches!(step, WatchStep::Resubscribe) {
        reconcile(client, socket, watch).await;
    }
    step
}

async fn project_hint(
    client: &Client,
    socket: &Path,
    watch: &mut ProjectionWatch,
    id: &str,
    seq: u64,
) {
    let Ok(runs) = client.open_runs() else {
        error!("mission projector could not list open runs");
        return;
    };
    watch.note_runs(&runs);
    let mut matching = runs
        .iter()
        .filter(|run| tracked_id(run) == Some(id))
        .peekable();
    if matching.peek().is_none() {
        watch.forget(id);
        return;
    }
    let Ok(mut conn) = Connection::connect(socket).await else {
        return;
    };
    for run in matching {
        if let Err(err) = project_bound(client, watch, &mut conn, run, seq).await {
            error!("mission projector exact observation: {err}");
        }
    }
}

fn tracked_id(run: &OpenRun) -> Option<&str> {
    run.execution.as_ref()?;
    run.terminal_id.as_deref()
}

fn agent_update_id(item: &WatchItem) -> Option<String> {
    let WatchItem::AgentState(update) = item else {
        return None;
    };
    terminal_key(update.terminal.as_ref()?)
}

async fn apply_live(client: &Client, socket: &Path, watch: &mut ProjectionWatch, item: &WatchItem) {
    if let Some(id) = agent_update_id(item)
        && watch.tracks(client, &id, false)
    {
        project_hint(client, socket, watch, &id, 0).await;
    }
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
    let mut conn = Connection::connect(socket)
        .await
        .map_err(|err| err.to_string())?;
    let authority = super::mission_start::owner_on(&mut conn).await?;
    let runs = client
        .open_runs()
        .map_err(|err| format!("open runs: {err}"))?;
    watch.note_runs(&runs);
    for run in &runs {
        if let Err(err) = project_bound(client, watch, &mut conn, run, 0).await {
            error!("mission projector reconcile: {err}");
        }
    }
    drain_queue(client, socket, watch, authority);
    Ok(())
}

async fn project_bound(
    client: &Client,
    watch: &mut ProjectionWatch,
    conn: &mut Connection,
    run: &OpenRun,
    seq: u64,
) -> Result<(), String> {
    let (agent, terminal) = if let Some(pin) = &run.execution {
        let Some(facts) = observe(conn, pin).await? else {
            return Ok(());
        };
        facts
    } else if run.session_key.is_some() {
        // Legacy pane IDs are only hints and may refer to unrelated work.
        (None, TerminalFact::Uncertain)
    } else {
        return Ok(());
    };
    let input = ProjectionInput {
        current: run.state,
        run: phase(run),
        agent,
        terminal: Some(terminal),
        binding_present: true,
        credential_lost: run.credential_lost,
        launching: false,
    };
    write_projection(client, watch, run, &project(&input), seq).map_err(|err| err.to_string())
}

fn write_projection(
    client: &Client,
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
    if applied.objective.state.frees_slot()
        && let Some(id) = &run.terminal_id
    {
        watch.forget(id);
    }
    Ok(())
}

fn drain_queue(
    client: &Client,
    socket: &Path,
    watch: &mut ProjectionWatch,
    authority: AuthorityId,
) {
    let Ok(runs) = client.open_runs() else {
        return;
    };
    let Some(queued) = runs
        .iter()
        .find(|run| run.run_state == RunState::Queued && run.authority == Some(authority))
    else {
        return;
    };
    let running = runs
        .iter()
        .filter(|run| run.run_state == RunState::Running)
        .count();
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
