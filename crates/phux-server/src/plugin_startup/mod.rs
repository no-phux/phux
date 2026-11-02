//! Server-supervised `[[startup]]` sidecars.
//!
//! One task on the server `LocalSet` owns every sidecar. Each child is a
//! process-group leader. The task starts after the accept loop is polling,
//! including on an upgrade resume, and it is stopped before the `LocalSet`
//! drops, before `execve` in an upgrade, and when a plugin is disabled.
//! Sidecars are not part of the upgrade `StateBlob`.

mod backoff;
mod exit_watch;
mod plan;
mod reap;
mod rotate;
mod spawn;
mod stop;
mod supervise;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::state::SharedState;

pub use plan::{StartupCatalog, StartupSpec, catalog_from_plugins};
use supervise::Cmd;

/// How the supervisor re-reads `[[plugins]]` on a config-reload doorbell.
#[derive(Clone)]
pub struct PluginStartupSource(Arc<dyn Fn() -> Result<StartupCatalog, String> + Send + Sync>);

impl std::fmt::Debug for PluginStartupSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PluginStartupSource(..)")
    }
}

impl PluginStartupSource {
    /// Wrap the function a reload calls. Errors keep the running sidecars.
    #[must_use]
    pub fn new(read: impl Fn() -> Result<StartupCatalog, String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(read))
    }

    pub(super) fn read(&self) -> Result<StartupCatalog, String> {
        (self.0)()
    }
}

/// What the server was asked to supervise, plus where the files go.
#[derive(Clone, Debug)]
pub struct PluginStartupConfig {
    /// Catalog at process start. A reload replaces it from [`Self::reload`].
    pub catalog: StartupCatalog,
    /// Profile state directory. Logs, pidfiles, and `PHUX_PLUGIN_STATE` live
    /// under `plugin-startup/` here. Lifecycle lines append to
    /// `plugin-runs.jsonl` in this directory.
    pub state_dir: PathBuf,
    /// Re-read on the config-reload doorbell. `None` keeps the start catalog.
    pub reload: Option<PluginStartupSource>,
}

/// Build the config the `phux server` process supervises.
///
/// The reload closure re-reads `config_path` the same way startup did.
#[must_use]
pub fn config_from(
    config_path: &Path,
    plugins: &[phux_config::plugin::PluginConfigEntry],
    state_dir: &Path,
) -> PluginStartupConfig {
    let catalog = catalog_from_plugins(config_path, plugins);
    let path = config_path.to_path_buf();
    PluginStartupConfig {
        catalog,
        state_dir: state_dir.to_path_buf(),
        reload: Some(PluginStartupSource::new(move || {
            let cfg = phux_config::loader::load_from(&path).map_err(|err| err.to_string())?;
            Ok(catalog_from_plugins(&path, &cfg.plugins))
        })),
    }
}

/// Handle stored on the server so upgrade and config reload can reach the
/// supervisor without waiting on it while holding the state lock.
#[derive(Clone, Debug)]
pub struct StartupHandle {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl StartupHandle {
    /// Ask the supervisor to re-read its source. Does not wait for the
    /// reconcile; the doorbell ack only means the request was queued.
    pub(crate) fn request_reload(&self) {
        let _ = self.tx.send(Cmd::Reload);
    }

    /// Stop every sidecar and wait until the grace has finished.
    pub(crate) async fn pause(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::Pause(tx)).is_err() {
            return;
        }
        let _ =
            tokio::time::timeout(stop::STOP_GRACE + std::time::Duration::from_secs(3), rx).await;
    }

    /// Start the catalog again after [`Self::pause`], if upgrade did not exec.
    pub(crate) fn resume(&self) {
        let _ = self.tx.send(Cmd::Resume);
    }
}

/// Spawn the supervisor on the current `LocalSet` and store its handle.
/// The caller awaits the join handle before dropping the `LocalSet`, after
/// the root token is cancelled, so the grace period finishes first.
pub(crate) fn install(
    state: &SharedState,
    cfg: PluginStartupConfig,
    socket: PathBuf,
    root: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let (tx, rx) = mpsc::unbounded_channel();
    state.with_mut(|server| server.set_plugin_startup(StartupHandle { tx }));
    tokio::task::spawn_local(async move {
        supervise::run(rx, cfg, socket, root).await;
    })
}

/// Stop sidecars before upgrade `exec`. No-op when none are supervised.
pub(crate) async fn pause(state: &SharedState) {
    let handle = state.with(|server| server.plugin_startup().cloned());
    if let Some(handle) = handle {
        handle.pause().await;
    }
}

/// Start sidecars again after a failed upgrade prepare or exec.
pub(crate) fn resume(state: &SharedState) {
    if let Some(handle) = state.with(|server| server.plugin_startup().cloned()) {
        handle.resume();
    }
}

/// Queue a reconcile from the config-reload doorbell.
pub(crate) fn request_reload(state: &SharedState) {
    if let Some(handle) = state.with(|server| server.plugin_startup().cloned()) {
        handle.request_reload();
    }
}
