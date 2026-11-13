//! Standing mirrors (`[[mirrors]]`, `docs/resource-kinds.md#standing-mirrors`).
//!
//! One task on the server `LocalSet` keeps a `phux agent surface mirror`
//! running for each entry, in a pane of its own ephemeral session named by
//! [`MirrorConfigEntry::session_name`]. The pane is the surface's parent, so
//! the surface is listed and granted like any other mirror's. A hot upgrade
//! carries the pane and the mirror re-adopts its surface by publishing; the
//! resumed image's task finds the session by name and leaves it. A pane that
//! exits is replaced after a backoff; a cold start recreates every one. An
//! adopted `phux` mirror or producer whose binary was replaced since it
//! started (`phux update`) is restarted on the installed one, so new input
//! intents reach it; it re-adopts the same surface. A
//! config reload re-reads the entries: a removed entry's session is closed,
//! a changed one restarted. On macOS the server's children cannot hold
//! Screen Recording access (TCC), so the pane only holds the session and
//! Rune runs the mirror into it (`phux agent surface standing`), which
//! makes the TCC prompts Rune's.
//!
//! `[[producers]]` (`docs/applications.md#standing-producers`) rides the
//! same loop: each entry's command runs in a `producer-<name>` session and
//! is restarted, adopted and reloaded exactly like a mirror.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use phux_config::mirror::MIRROR_SESSION_PREFIX;
use phux_config::producer::PRODUCER_SESSION_PREFIX;
use phux_config::{MirrorConfigEntry, ProducerConfigEntry};
use phux_terminal_api::actor::TerminalResource as _;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::commands::seed::{SpawnOwnership, seed_session, spawn_pane_with_pty_and_colors};
use crate::runtime::commands::{SeedOrigin, SpawnAttribution};
use crate::runtime::standing::{MirrorSource, Slot, StandingEntries, TICK};
use crate::state::SharedState;

/// What a macOS mirror session's pane runs while Rune mirrors into it.
const MACOS_HOLDER: &str = "echo 'Rune keeps this mirror: macOS gives Screen Recording to the app that captures. Keep Rune open.'; exec tail -f /dev/null";

/// Start the supervisor for `source` on the current `LocalSet`.
pub(crate) fn install(state: &SharedState, source: &MirrorSource, root_token: &CancellationToken) {
    let source = source.clone();
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
        let outdated = outdated(&state, &units, &slots).await;
        tick(&state, units, &mut slots, &outdated, Instant::now(), &root_token);
        tokio::select! {
            () = root_token.cancelled() => return,
            () = tokio::time::sleep(TICK) => {}
            () = reload.notified() => entries = read(&source, entries),
        }
    }
}

/// The entries `source` names, or `previous` when it cannot be read.
fn read(source: &MirrorSource, previous: StandingEntries) -> StandingEntries {
    match source.read() {
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

/// The adopted (found running, not started here) `phux` sessions whose
/// process runs a binary replaced since it started.
async fn outdated(
    state: &SharedState,
    wanted: &BTreeMap<String, Vec<String>>,
    slots: &BTreeMap<String, Slot>,
) -> BTreeSet<String> {
    let Ok(installed) = state.with(crate::state::ServerState::native_host_executable) else {
        return BTreeSet::new();
    };
    let mut outdated = BTreeSet::new();
    for (name, argv) in wanted {
        let ours = argv.first().is_some_and(|word| word == "phux")
            && !(cfg!(target_os = "macos") && name.starts_with(MIRROR_SESSION_PREFIX));
        if !ours || slots.get(name).is_some_and(Slot::was_started) {
            continue;
        }
        for terminal in terminals(state, name) {
            let process = super::commands::query_process_facet(&terminal).await;
            let Some(child) = process.and_then(|p| p.exit.is_none().then_some(p.child?)) else {
                continue;
            };
            if phux_resource::proc_query::runs_replaced_executable(child.pid, &installed)
                == Some(true)
            {
                outdated.insert(name.clone());
            }
        }
    }
    outdated
}

/// The terminal handles of `name`'s panes.
fn terminals(state: &SharedState, name: &str) -> Vec<phux_terminal_api::actor::TerminalHandle> {
    state.with(|s| {
        let Some(session) = s.find_session_by_name(name) else {
            return Vec::new();
        };
        s.session_terminals(session)
            .into_iter()
            .filter_map(|pane| s.resource_handle(pane)?.terminal().ok().cloned())
            .collect()
    })
}

fn tick(
    state: &SharedState,
    wanted: BTreeMap<String, Vec<String>>,
    slots: &mut BTreeMap<String, Slot>,
    outdated: &BTreeSet<String>,
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
        if alive && (slot.changed(&args) || outdated.contains(&name)) {
            if outdated.contains(&name) {
                info!(session = %name, "standing session runs a replaced phux binary; restarting it");
            } else {
                info!(session = %name, "standing mirror changed; restarting it");
            }
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
                tick(&state, wanted, &mut slots, &BTreeSet::new(), Instant::now(), &token);
                for name in names {
                    assert!(live(&state, name), "{name} started");
                }
                tick(&state, BTreeMap::new(), &mut slots, &BTreeSet::new(), Instant::now(), &token);
                let mut gone = false;
                for _ in 0..100 {
                    gone =
                        state.with(|s| names.iter().all(|n| s.find_session_by_name(n).is_none()));
                    if gone {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                assert!(gone, "removed entries' sessions close");
                token.cancel();
            })
            .await;
    }

    /// An adopted `phux` producer keeps running until `phux update` replaces
    /// the binary under it; then it is restarted on the installed one. One
    /// this supervisor started is left alone.
    #[tokio::test(flavor = "current_thread")]
    async fn an_adopted_producer_on_a_replaced_binary_restarts() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("phux");
        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("sleep");
        // macOS kills a copied system binary: link it, swap in a script.
        std::os::unix::fs::symlink(sleep, &fake).expect("link");
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let state = SharedState::new();
                state.set_terminal_engine(Arc::new(phux_terminal::TerminalEngine));
                state.with_mut(|s| s.set_native_host_executable(Some(fake.clone())));
                let token = CancellationToken::new();
                let producer = ProducerConfigEntry {
                    name: "feed".to_owned(),
                    command: vec!["phux".to_owned(), "30".to_owned()],
                };
                let wanted = units(&[], &[producer]);
                let name = "producer-feed";
                let none = BTreeSet::new();
                let mut started = BTreeMap::new();
                tick(&state, wanted.clone(), &mut started, &none, Instant::now(), &token);
                assert!(live(&state, name));
                let panes = |state: &SharedState| {
                    state.with(|s| s.session_terminals(s.find_session_by_name(name).unwrap()))
                };
                let first = panes(&state);
                // A hot upgrade's supervisor has no slot for what it adopts.
                let adopted = BTreeMap::new();
                assert!(outdated(&state, &wanted, &adopted).await.is_empty());
                let next = dir.path().join("phux.new");
                std::fs::write(&next, "#!/bin/sh\nexec sleep 30\n").expect("write");
                std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
                std::fs::rename(&next, &fake).expect("swap");
                assert!(outdated(&state, &wanted, &started).await.is_empty(), "started here");
                let found = outdated(&state, &wanted, &adopted).await;
                assert_eq!(found.iter().map(String::as_str).collect::<Vec<_>>(), [name]);
                let mut adopted = adopted;
                tick(&state, wanted, &mut adopted, &found, Instant::now(), &token);
                assert!(live(&state, name), "restarted");
                assert_ne!(panes(&state), first, "on a new pane");
                token.cancel();
            })
            .await;
    }
}
