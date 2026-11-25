//! `[restore] scrollback`: each pane's last lines in an owner-only file, so
//! a cold start can show them again (`docs/security.md`, "Kept scrollback").
//!
//! The engine hands over the primary screen's text ([`Keeper::ask`]); a
//! thread writes it. Files sit beside the organization store and are named
//! by resource key, so each server owns exactly its own. Text is reduced to
//! printable characters, newlines and SGR ([`clean`]) on the way to disk and
//! again on the way back, because anything running as the user can edit it.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Read as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use phux_core::ids::ResourceId;
use phux_core::{LaunchSpec, Resource, ResourceKey};
use phux_terminal_api::{KEPT_SCROLLBACK_MAX_BYTES, KeepScrollback};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::state::{AppSlot, ServerState, SharedState};

mod text;
use text::{clean, compose, divider};

/// How soon the file of a removed or newly excluded pane is deleted.
const SWEEP: Duration = Duration::from_secs(1);
/// How long a stopping server waits for its last saves.
const FINISH: Duration = Duration::from_secs(5);

/// The keeper, once [`start`] armed it, and its thread's end.
#[derive(Debug, Default)]
struct KeepSlot(Option<(Arc<Keeper>, oneshot::Receiver<()>)>);

impl AppSlot for KeepSlot {
    fn forget_resource(
        &mut self,
        _resource: ResourceId,
        _wire: Option<&phux_protocol::ids::ResourceId>,
    ) {
    }
}

/// Where the kept lines are, and the writes its thread still owes.
#[derive(Debug)]
pub(crate) struct Keeper {
    dir: PathBuf,
    lines: u32,
    exclude: Vec<String>,
    queue: Mutex<Queue>,
    wake: Condvar,
}

#[derive(Debug, Default)]
struct Queue {
    /// The newest text per pane, or `None` to delete its file.
    pending: BTreeMap<ResourceKey, Option<Vec<u8>>>,
    /// Panes that have a file, or are about to.
    known: HashSet<ResourceKey>,
    stop: bool,
}

/// Arm the keeper for the organization store at `store`, before any engine
/// is built. With the setting off, delete what an earlier run kept instead.
pub(crate) fn start(state: &SharedState, store: Option<&Path>, root_token: &CancellationToken) {
    let restore = state.with(|s| s.config().restore.clone());
    // Panes of a store kept in memory have no key to come back to.
    let Some(dir) = store.map(|store| store.with_extension("scrollback")) else {
        return;
    };
    if restore.scrollback == 0 && std::fs::symlink_metadata(&dir).is_err() {
        return;
    }
    if let Err(refusal) = phux_config::production::refuse_dev_on_production_state(&dir) {
        error!("scrollback not kept: {refusal}");
        return;
    }
    if restore.scrollback == 0 {
        tokio::task::spawn_blocking(move || {
            if let Err(err) = std::fs::remove_dir_all(&dir) {
                error!(dir = %dir.display(), error = %err, "kept scrollback not deleted");
            }
        });
        return;
    }
    let known = match open_dir(&dir).and_then(|()| known_files(&dir)) {
        Ok(known) => known,
        Err(err) => {
            error!(dir = %dir.display(), error = %err, "scrollback not kept");
            return;
        }
    };
    let keeper = Arc::new(Keeper {
        dir,
        lines: restore.scrollback,
        exclude: restore.scrollback_exclude,
        queue: Mutex::new(Queue {
            known,
            ..Queue::default()
        }),
        wake: Condvar::new(),
    });
    let (done_tx, done_rx) = oneshot::channel();
    let writer = Arc::clone(&keeper);
    let spawned = std::thread::Builder::new()
        .name("phux-scrollback".to_owned())
        .spawn(move || {
            writer.run();
            let _ = done_tx.send(());
        });
    if let Err(err) = spawned {
        error!(error = %err, "scrollback not kept: could not start its thread");
        return;
    }
    state.with_mut(|s| {
        s.app_slot_mut::<KeepSlot>().0 = Some((Arc::clone(&keeper), done_rx));
    });
    tokio::task::spawn_local(sweep(state.clone(), keeper, root_token.clone()));
}

/// Write what is still owed, once every engine has stopped.
pub(crate) async fn finish(state: &SharedState) {
    let Some((keeper, done)) = state.with_mut(|s| s.app_slot_mut::<KeepSlot>().0.take()) else {
        return;
    };
    // A stopping server removes no records, so only what was deleted goes.
    keeper.sweep(state);
    keeper.queue().stop = true;
    keeper.wake.notify_one();
    if tokio::time::timeout(FINISH, done).await.is_err() {
        warn!("stopping before every pane's scrollback was saved");
    }
}

/// The armed keeper, if any.
pub(crate) fn keeper(state: &SharedState) -> Option<Arc<Keeper>> {
    state.with(|s| {
        let (keeper, _) = s.app_slot::<KeepSlot>()?.0.as_ref()?;
        Some(Arc::clone(keeper))
    })
}

/// What a new engine for `resource` shows before its program's first
/// output: its kept lines and a divider, all in history. The `rows`-tall
/// screen starts blank, so nothing that reads the screen (agent rules,
/// `phux wait`) takes old text for new.
// ponytail: reads the file on the runtime thread, once per relaunch beside
// a fork; move it to the keeper's thread if restoring many panes stalls.
pub(crate) fn seed(state: &SharedState, resource: ResourceId, rows: u16) -> Vec<u8> {
    let Some(keeper) = keeper(state) else {
        return Vec::new();
    };
    let Some(key) = state.with(|s| keeper.kept_key(s, resource)) else {
        return Vec::new();
    };
    if !keeper.queue().known.contains(&key) {
        return Vec::new();
    }
    match read(&keeper.path(key)) {
        Ok(text) => compose(&text, keeper.lines, rows, &divider()),
        Err(err) => {
            warn!(%key, error = %err, "kept scrollback not shown");
            Vec::new()
        }
    }
}

/// Delete the files of panes that are gone or excluded, now and as they
/// become so. The first pass catches what a crash left behind.
async fn sweep(state: SharedState, keeper: Arc<Keeper>, token: CancellationToken) {
    let mut tick = tokio::time::interval(SWEEP);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = token.cancelled() => return,
            _ = tick.tick() => keeper.sweep(&state),
        }
    }
}

impl Keeper {
    /// What the engine of `resource` is asked to keep.
    pub(crate) fn ask(
        self: &Arc<Self>,
        state: &SharedState,
        resource: ResourceId,
    ) -> KeepScrollback {
        let keeper = Arc::clone(self);
        let state = state.clone();
        KeepScrollback {
            lines: self.lines,
            save: Rc::new(move |text| {
                // A killed pane's engine still hands over its last screen.
                if let Some(key) = state.with(|s| keeper.kept_key(s, resource)) {
                    keeper.push(key, Some(text));
                }
            }),
        }
    }

    /// The key `resource` is kept under, unless it is gone or excluded.
    fn kept_key(&self, s: &ServerState, resource: ResourceId) -> Option<ResourceKey> {
        let record = s.registry().record(resource)?;
        (!self.excludes(s, record)).then_some(record.key)
    }

    fn sweep(&self, state: &SharedState) {
        let mut gone: Vec<ResourceKey> = self.queue().known.iter().copied().collect();
        state.with(|s| {
            gone.retain(|key| s.org().resource(key).is_none_or(|r| self.excludes(s, r)));
        });
        for key in gone {
            self.push(key, None);
        }
    }

    /// `restore.scrollback-exclude` names `record`'s session, the pane, or
    /// the program it was started with.
    fn excludes(&self, s: &ServerState, record: &Resource) -> bool {
        let session = s.org().session(&record.session).map(|s| s.name.as_str());
        let program = match &record.launch {
            Some(LaunchSpec::Terminal(launch)) => launch
                .argv
                .first()
                .and_then(|program| program.rsplit('/').next()),
            _ => None,
        };
        self.exclude
            .iter()
            .any(|name| [session, record.name.as_deref(), program].contains(&Some(name.as_str())))
    }

    fn push(&self, key: ResourceKey, text: Option<Vec<u8>>) {
        let mut queue = self.queue();
        if text.is_some() {
            queue.known.insert(key);
        } else {
            queue.known.remove(&key);
        }
        queue.pending.insert(key, text);
        drop(queue);
        self.wake.notify_one();
    }

    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn path(&self, key: ResourceKey) -> PathBuf {
        self.dir.join(key.to_string())
    }

    /// The writer thread: every queued write, then the next, until stopped.
    fn run(&self) {
        loop {
            let mut queue = self.queue();
            let (key, text) = loop {
                if let Some(next) = queue.pending.pop_first() {
                    break next;
                }
                if queue.stop {
                    return;
                }
                queue = self
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            };
            drop(queue);
            if let Err(err) = self.write(key, text.as_deref()) {
                warn!(%key, error = %err, "scrollback file not updated");
            }
        }
    }

    fn write(&self, key: ResourceKey, text: Option<&[u8]>) -> io::Result<()> {
        let path = self.path(key);
        let text = text
            .and_then(|text| std::str::from_utf8(text).ok())
            .map(|text| clean(text, self.lines))
            .filter(|text| !text.is_empty());
        let Some(text) = text else {
            return remove(&path);
        };
        open_dir(&self.dir)?;
        crate::autosave::write_atomic(&path, text.as_bytes())
    }
}

/// Make `dir` an owner-only directory of ours; refuse a link or a directory
/// another user owns.
fn open_dir(dir: &Path) -> io::Result<()> {
    crate::owner_dir::create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != nix::unistd::geteuid().as_raw() {
        return Err(io::Error::other("not a directory this user owns"));
    }
    crate::owner_dir::tighten(dir)
}

/// The panes `dir` has files for. Anything else in it (an interrupted
/// write) is removed.
fn known_files(dir: &Path) -> io::Result<HashSet<ResourceKey>> {
    let mut known = HashSet::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        match entry.file_name().to_str().map(str::parse) {
            Some(Ok(key)) => {
                known.insert(key);
            }
            _ => remove(&entry.path())?,
        }
    }
    Ok(known)
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// A kept file's text: a regular file of ours, bounded, UTF-8.
fn read(path: &Path) -> io::Result<String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        // A pipe squatting on the name must not block the open.
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    let limit = KEPT_SCROLLBACK_MAX_BYTES as u64;
    if !meta.is_file() || meta.uid() != nix::unistd::geteuid().as_raw() || meta.len() > limit {
        return Err(io::Error::other("not a file phux wrote"));
    }
    let mut text = String::new();
    file.take(limit).read_to_string(&mut text)?;
    Ok(text)
}

#[cfg(test)]
mod tests;
