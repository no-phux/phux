//! Standing mirrors (`[[mirrors]]`, `docs/resource-kinds.md#standing-mirrors`).
//!
//! One task on the server `LocalSet` keeps a `phux agent surface mirror`
//! running for each entry, in a pane of its own ephemeral session named by
//! [`MirrorConfigEntry::session_name`]. The pane is the surface's parent, so
//! the surface is listed and granted like any other mirror's. A hot upgrade
//! carries the pane and the mirror re-adopts its surface by publishing; the
//! resumed image's task finds the session by name and leaves it. A pane that
//! exits is replaced after a backoff; a cold start recreates every one. A
//! config reload re-reads the entries: a removed entry's session is closed,
//! a changed one restarted. On macOS the server's children cannot hold
//! Screen Recording access (TCC), so the pane only holds the session and
//! Rune runs the mirror into it (`phux agent surface standing`), which
//! makes the TCC prompts Rune's.
//!
//! `[[producers]]` (`docs/applications.md#standing-producers`) rides the
//! same loop: each entry's command runs in a `producer-<name>` session and
//! is restarted, adopted and reloaded exactly like a mirror.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_config::mirror::MIRROR_SESSION_PREFIX;
use phux_config::producer::PRODUCER_SESSION_PREFIX;
use phux_config::{MirrorConfigEntry, ProducerConfigEntry};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::hub::link::Backoff;
use crate::runtime::apps::terminal::commands::seed::{
    SpawnOwnership, seed_session, spawn_pane_with_pty_and_colors,
};
use crate::runtime::apps::terminal::commands::{SeedOrigin, SpawnAttribution};
use crate::state::SharedState;

mod demand;
pub(crate) use demand::{handle, provide};

/// What the supervisor keeps: `[[mirrors]]` and `[[producers]]`.
pub type StandingEntries = (Vec<MirrorConfigEntry>, Vec<ProducerConfigEntry>);

/// How the supervisor reads its entries, at start and on every reload.
#[derive(Clone)]
pub struct MirrorSource(Arc<dyn Fn() -> Result<StandingEntries, String> + Send + Sync>);

impl std::fmt::Debug for MirrorSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MirrorSource(..)")
    }
}

impl MirrorSource {
    /// Wrap the function a start and a reload call. A reload error keeps
    /// the running mirrors.
    #[must_use]
    pub fn new(read: impl Fn() -> Result<StandingEntries, String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(read))
    }
}

/// How often the supervisor looks for a mirror that exited.
const TICK: Duration = Duration::from_secs(2);
/// A mirror that ran this long clears its failure streak.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// What a macOS mirror session's pane runs while Rune mirrors into it.
const MACOS_HOLDER: &str = "echo 'Rune keeps this mirror: macOS gives Screen Recording to the app that captures. Keep Rune open.'; exec tail -f /dev/null";

/// Start the supervisor for `source` on the current `LocalSet`.
pub(crate) fn install(
    state: &SharedState,
    source: Option<MirrorSource>,
    root_token: &CancellationToken,
) {
    let Some(source) = source else {
        return;
    };
    let reload = Arc::new(Notify::new());
    state.with_mut(|s| s.set_standing_mirrors(reload.clone()));
    tokio::task::spawn_local(supervise(state.clone(), source, reload, root_token.clone()));
}

async fn supervise(
    state: SharedState,
    source: MirrorSource,
    reload: Arc<Notify>,
    root_token: CancellationToken,
) {
    let mut entries = read(&source, StandingEntries::default());
    let mut slots = BTreeMap::new();
    loop {
        state.with_mut(|s| s.mirror_demand_mut().config.clone_from(&entries.0));
        let mirrors = state.with(|s| s.mirror_demand().wanted());
        let units = units(&mirrors, &entries.1);
        tick(&state, units, &mut slots, Instant::now(), &root_token);
        tokio::select! {
            () = root_token.cancelled() => return,
            () = tokio::time::sleep(TICK) => {}
            () = reload.notified() => entries = read(&source, entries),
        }
    }
}

/// The entries `source` names, or `previous` when it cannot be read.
fn read(source: &MirrorSource, previous: StandingEntries) -> StandingEntries {
    match (source.0)() {
        Ok(entries) if cfg!(target_os = "macos") && !entries.0.is_empty() => {
            info!(
                "[[mirrors]]: a macOS server cannot hold Screen Recording access; Rune runs the mirrors into their sessions while it is open"
            );
            entries
        }
        Ok(entries) => entries,
        Err(error) => {
            warn!(%error, "[[mirrors]] reload failed; keeping the running mirrors");
            previous
        }
    }
}

/// One entry's restart bookkeeping; Rune's `phux agent surface standing`
/// keeps the same for the mirrors it runs on macOS.
#[derive(Debug)]
pub struct Slot {
    backoff: Backoff,
    /// When this supervisor last started the mirror; `None` for one it
    /// found running (a hot upgrade) or has not started.
    started: Option<Instant>,
    /// Not before this instant may it start again.
    next: Option<Instant>,
    /// The arguments it was started with; a reload that changes them
    /// restarts it.
    args: Option<Vec<String>>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            backoff: Backoff::new(BACKOFF_BASE, BACKOFF_CAP),
            started: None,
            next: None,
            args: None,
        }
    }
}

impl Slot {
    /// Record a start at `now` with `args`.
    pub fn started(&mut self, now: Instant, args: Vec<String>) {
        self.started = Some(now);
        self.args = Some(args);
    }

    /// Whether a running mirror was started with other arguments than
    /// `args`, so a reload changed it.
    #[must_use]
    pub fn changed(&self, args: &[String]) -> bool {
        self.args.as_deref().is_some_and(|old| old != args)
    }

    /// Whether to start the mirror now, given whether it runs.
    pub fn due(&mut self, alive: bool, now: Instant) -> bool {
        if alive {
            return false;
        }
        if let Some(started) = self.started.take() {
            if now.saturating_duration_since(started) >= HEALTHY_AFTER {
                self.backoff.reset();
            }
            self.next = Some(now + self.backoff.next_delay());
        }
        self.next.is_none_or(|next| now >= next)
    }
}

/// Every standing session to keep, by name, with the argv it runs; a first
/// word `phux` is this server's own binary.
fn units(
    mirrors: &[MirrorConfigEntry],
    producers: &[ProducerConfigEntry],
) -> BTreeMap<String, Vec<String>> {
    let mirrors = mirrors.iter().map(|entry| {
        let mut argv = vec!["phux".to_owned()];
        argv.extend(entry.args());
        (entry.session_name(), argv)
    });
    let producers = producers.iter().filter_map(|entry| {
        if entry.command.first().is_none_or(String::is_empty) {
            warn!(producer = %entry.name, "[[producers]] entry has no command; skipping it");
            return None;
        }
        Some((entry.session_name(), entry.command.clone()))
    });
    let mut wanted = BTreeMap::new();
    for (name, argv) in mirrors.chain(producers) {
        if wanted.insert(name.clone(), argv).is_some() {
            warn!(session = %name, "two standing entries name the same session; keeping the last");
        }
    }
    wanted
}

fn tick(
    state: &SharedState,
    wanted: BTreeMap<String, Vec<String>>,
    slots: &mut BTreeMap<String, Slot>,
    now: Instant,
    root_token: &CancellationToken,
) {
    let owned: Vec<String> = state.with(|s| {
        s.registry()
            .sessions()
            .map(|(_, session)| session.name.clone())
            .filter(|name| {
                name.starts_with(MIRROR_SESSION_PREFIX) || name.starts_with(PRODUCER_SESSION_PREFIX)
            })
            .collect()
    });
    for name in owned.iter().filter(|name| !wanted.contains_key(*name)) {
        info!(session = %name, "standing session removed from config; stopping it");
        stop(state, name);
    }
    slots.retain(|name, _| wanted.contains_key(name));
    for (name, args) in wanted {
        let slot = slots.entry(name.clone()).or_default();
        let mut alive = live(state, &name);
        if alive && slot.changed(&args) {
            info!(session = %name, "standing mirror changed; restarting it");
            stop(state, &name);
            *slot = Slot::default();
            alive = false;
        }
        if slot.due(alive, now) {
            slot.started(now, args.clone());
            if let Err(error) = start(state, &name, args, root_token) {
                warn!(session = %name, %error, "standing mirror failed to start");
            }
        }
    }
}

/// Whether `name` holds a pane whose process still runs.
fn live(state: &SharedState, name: &str) -> bool {
    state.with(|s| {
        s.find_session_by_name(name).is_some_and(|session| {
            s.session_terminals(session)
                .into_iter()
                .any(|pane| s.resource_handle(pane).is_some() && s.retained_exit(pane).is_none())
        })
    })
}

/// Close every pane of `name`; its ephemeral session goes with the last.
fn stop(state: &SharedState, name: &str) {
    state.with_mut(|s| {
        let Some(session) = s.find_session_by_name(name) else {
            return;
        };
        let panes: Vec<_> = s
            .session_terminals(session)
            .into_iter()
            .filter(|pane| s.resource_handle(*pane).is_some())
            .collect();
        for pane in &panes {
            s.mark_terminating(*pane);
        }
        let _ = s.mark_and_cancel(
            &panes,
            phux_protocol::wire::frame::CloseReason::Killed,
            crate::state::CloseAttribution::default(),
        );
    });
}

/// Start the mirror in a new session `name`, or in the existing one a
/// retained exit or a cold start left behind, after removing its ended panes.
fn start(
    state: &SharedState,
    name: &str,
    args: Vec<String>,
    root_token: &CancellationToken,
) -> Result<(), String> {
    let command = if cfg!(target_os = "macos") && name.starts_with(MIRROR_SESSION_PREFIX) {
        // `phux agent surface standing` (Rune's child) mirrors into this pane.
        drop(args);
        ["/bin/sh", "-c", MACOS_HOLDER].map(str::to_owned).into()
    } else if args.first().is_some_and(|word| word == "phux") {
        let exe = state
            .with(crate::state::ServerState::native_host_executable)
            .map_err(|err| err.to_string())?;
        let mut command = vec![exe.to_string_lossy().into_owned()];
        command.extend(args.into_iter().skip(1));
        command
    } else {
        args
    };
    let cmd = crate::runtime::apps::terminal::launch::named_session_command(
        state,
        name,
        Some(command),
        None,
        BTreeMap::new(),
    );
    let scrollback = state.with(crate::state::ServerState::scrollback_limits);
    let existing = state.with_mut(|s| {
        // A cold start leaves the previous run's ended panes in the session.
        let session = s.find_session_by_name(name)?;
        let ended: Vec<_> = s
            .registry()
            .session(session)?
            .members
            .iter()
            .filter(|key| {
                s.registry()
                    .resource_id(key)
                    .is_none_or(|id| s.resource_handle(id).is_none())
            })
            .copied()
            .collect();
        for resource in ended {
            if let Err(refusal) = s.commit_system(phux_core::Op::ResourceRemove { resource }) {
                warn!(session = %name, %refusal, "an ended standing mirror pane was not removed");
            }
        }
        s.find_session_by_name(name)
    });
    let started = match existing {
        Some(session) => spawn_pane_with_pty_and_colors(
            state,
            &SpawnOwnership::Session(session),
            cmd,
            scrollback,
            root_token,
            None,
            None,
            None,
            SpawnAttribution::default(),
        )
        .map(|_| ()),
        None => seed_session(
            state,
            name,
            Some(cmd),
            scrollback,
            root_token,
            None,
            SeedOrigin {
                lifetime: phux_core::SessionLifetime::Ephemeral,
                ..SeedOrigin::default()
            },
        )
        .map(|_| ()),
    };
    started.map_err(|err| err.to_string())?;
    info!(session = %name, "standing mirror started");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mirror_starts_at_once_and_restarts_with_backoff() {
        let t0 = Instant::now();
        let mut slot = Slot::default();
        assert!(slot.due(false, t0), "first start is immediate");
        slot.started = Some(t0);
        assert!(!slot.due(true, t0 + TICK), "running: nothing to do");
        // Dies quickly: waits 1 s, then 2 s after the next quick death.
        let died = t0 + TICK;
        assert!(!slot.due(false, died));
        assert!(!slot.due(false, died + Duration::from_millis(900)));
        assert!(slot.due(false, died + BACKOFF_BASE));
        slot.started = Some(died + BACKOFF_BASE);
        let died = died + BACKOFF_BASE + TICK;
        assert!(!slot.due(false, died));
        assert!(!slot.due(false, died + BACKOFF_BASE));
        assert!(slot.due(false, died + BACKOFF_BASE * 2));
        // A healthy run clears the streak.
        slot.started = Some(died);
        let died = died + HEALTHY_AFTER;
        assert!(!slot.due(false, died));
        assert!(slot.due(false, died + BACKOFF_BASE));
    }

    /// A reload that adds entries starts their sessions, a `phux` producer
    /// on the server's own binary and any other command as given; one that
    /// drops them closes the sessions.
    #[tokio::test(flavor = "current_thread")]
    async fn reload_adds_and_removes_standing_mirrors_and_producers() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("phux");
        std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let state = SharedState::new();
                state.set_terminal_engine(Arc::new(phux_terminal::TerminalEngine));
                state.with_mut(|s| s.set_native_host_executable(Some(fake)));
                let token = CancellationToken::new();
                let mirror = MirrorConfigEntry {
                    target: "display".to_owned(),
                    format: None,
                    quality: None,
                    max_width: None,
                    interval_ms: None,
                };
                let producer = |name: &str, command: &[&str]| ProducerConfigEntry {
                    name: name.to_owned(),
                    command: command.iter().map(|w| (*w).to_owned()).collect(),
                };
                let producers = [
                    producer("feed", &["phux", "agent", "doc", "notifications"]),
                    producer("Sleep", &["/bin/sh", "-c", "exec sleep 30"]),
                    producer("empty", &[]),
                ];
                let names = ["mirror-display", "producer-feed", "producer-sleep"];
                let wanted = units(&[mirror], &producers);
                assert_eq!(wanted.keys().map(String::as_str).collect::<Vec<_>>(), names);
                let mut slots = BTreeMap::new();
                tick(&state, wanted, &mut slots, Instant::now(), &token);
                for name in names {
                    assert!(live(&state, name), "{name} started");
                }
                tick(&state, BTreeMap::new(), &mut slots, Instant::now(), &token);
                let mut gone = false;
                for _ in 0..100 {
                    gone =
                        state.with(|s| names.iter().all(|n| s.find_session_by_name(n).is_none()));
                    if gone {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                assert!(gone, "removed entries' sessions close");
                token.cancel();
            })
            .await;
    }

    #[test]
    fn a_mirror_found_running_is_adopted_not_restarted() {
        let mut slot = Slot::default();
        assert!(!slot.due(true, Instant::now()));
        assert!(slot.started.is_none() && slot.args.is_none());
    }
}
