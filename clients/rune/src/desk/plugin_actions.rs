//! Desktop palette actions. The window captures context; workers only run
//! local, configured programs and never hold a connection or a view.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use phux_client_core::layout::wire::unique_entry_for_target;
use phux_client_runtime::control::{ControlPlane, Status};
use phux_plugin::{ActionContext, ActionSource, ActionTarget, ConfiguredAction, ConfiguredLink};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{ArrangementOwner, EntryTarget, ResourceRecord};

use crate::palette::{Item, Target};

use super::{Desk, NoticeKind};

type Catalog = Result<(Vec<ConfiguredAction>, Vec<ConfiguredLink>), String>;
type Finished = Result<(), String>;

pub(super) struct Actions {
    config: PathBuf,
    log: PathBuf,
    catalog: Vec<ConfiguredAction>,
    /// The `[[links]]` handlers a ⌘-click consults first.
    pub(super) links: Vec<ConfiguredLink>,
    loading: Option<Receiver<Catalog>>,
    running: Option<Receiver<Finished>>,
}

impl Default for Actions {
    fn default() -> Self {
        Self {
            config: phux_config_core::loader::config_path(),
            log: phux_plugin::run_log::default_path(),
            catalog: Vec::new(),
            links: Vec::new(),
            loading: None,
            running: None,
        }
    }
}

impl Actions {
    pub(super) fn items(&self) -> impl Iterator<Item = Item> + '_ {
        self.catalog.iter().map(|action| Item {
            title: action.title.clone(),
            detail: action.plugin.clone(),
            kind: "action",
            target: Target::Extension(action.clone()),
        })
    }
}

impl Desk {
    pub(super) fn refresh_plugin_actions(&mut self) {
        if self.plugin_actions.loading.is_some() || self.native.is_some() {
            return;
        }
        let path = self.plugin_actions.config.clone();
        match worker(self.waker.clone(), move || {
            let actions = phux_plugin::configured_actions(&path);
            let links = phux_plugin::configured_links(&path);
            actions
                .and_then(|a| Ok((a, links?)))
                .map_err(|error| error.to_string())
        }) {
            Ok(receiver) => self.plugin_actions.loading = Some(receiver),
            Err(why) => self.say(NoticeKind::Error, why),
        }
    }

    pub(super) fn run_plugin_action(&mut self, action: ConfiguredAction) {
        if self.plugin_actions.running.is_some() {
            return self.say(NoticeKind::Info, "A plugin action is already running.");
        }
        let request = phux_plugin::PluginActionRequest {
            plugin_id: action.plugin,
            action_id: action.id,
            cwd: None,
            timeout: Some(Duration::from_secs(30)),
            context: self.capture_plugin_context(),
        };
        let path = self.plugin_actions.config.clone();
        let log = self.plugin_actions.log.clone();
        match worker(self.waker.clone(), move || {
            let result = phux_plugin::run_configured_action_blocking(&path, &request, log);
            let output = result.map_err(|error| error.to_string())?;
            match (output.outcome, output.exit_code) {
                (phux_plugin::PluginActionOutcome::Completed, Some(0)) => Ok(()),
                (phux_plugin::PluginActionOutcome::Completed, Some(code)) => Err(format!(
                    "Plugin action exited with status {code}. See phux plugin log."
                )),
                (phux_plugin::PluginActionOutcome::Completed, None) => {
                    Err("Plugin action terminated by a signal. See phux plugin log.".into())
                }
                (phux_plugin::PluginActionOutcome::TimedOut, _) => {
                    Err("Plugin action timed out. See phux plugin log.".into())
                }
            }
        }) {
            Ok(receiver) => self.plugin_actions.running = Some(receiver),
            Err(why) => self.say(NoticeKind::Error, why),
        }
    }

    pub(super) fn pump_plugin_actions(&mut self) -> bool {
        let loaded = receive(&mut self.plugin_actions.loading);
        let finished = receive(&mut self.plugin_actions.running);
        let changed = loaded.is_some() || finished.is_some();
        if let Some(result) = loaded {
            self.install_action_catalog(result);
        }
        if let Some(result) = finished {
            match result {
                Ok(()) => self.say(NoticeKind::Info, "Plugin action completed."),
                Err(why) => self.say(NoticeKind::Error, why),
            }
        }
        changed
    }

    fn install_action_catalog(&mut self, result: Catalog) {
        let picked = (self.palette_open && self.palette_picked != 0)
            .then(|| {
                self.palette_hits()
                    .get(self.palette_picked)
                    .map(|hit| hit.item.target.clone())
            })
            .flatten();
        (self.plugin_actions.catalog, self.plugin_actions.links) = match result {
            Ok(catalog) => catalog,
            // Every ⌘ press reloads; only the palette reports a bad config.
            Err(why) => {
                if self.palette_open {
                    self.say(NoticeKind::Error, why);
                }
                Default::default()
            }
        };
        self.rehover();
        if let Some(picked) = picked {
            self.palette_picked = self
                .palette_hits()
                .iter()
                .position(|hit| hit.item.target == picked)
                .unwrap_or_default();
        }
    }

    /// Only coherent observations from this client incarnation are admitted.
    /// Nothing in the worker re-reads the window's selected pane or route.
    fn capture_plugin_context(&self) -> ActionContext {
        let snapshot = self.connection.client.as_ref().and_then(|client| {
            let focus = self.keyboard().map(|(id, _)| id);
            let tab = self.connection.tabs.list.iter().find(|tab| tab.active);
            client.with_control(|control| {
                if control.status() != Status::Attached
                    || control.connection_epoch() != self.connection.epoch
                {
                    return None;
                }
                let mut target = capture_target(control, focus, tab.map(|tab| tab.id));
                target.route = self.machines.current.clone().or_else(|| {
                    self.connection
                        .action_transport
                        .filter(|family| *family == "uds")
                        .map(|_| "local".to_owned())
                });
                target.transport = self
                    .connection
                    .action_transport
                    .map(|family| if family == "ws" { "websocket" } else { family })
                    .map(str::to_owned);
                let invoking = control.attached_client_id().map(|id| id.get());
                Some((invoking, target))
            })
        });
        let (client, target) = snapshot.unwrap_or_default();
        ActionContext::observed(ActionSource::RunePalette, client, target)
    }
}

#[cfg(test)]
mod tests;

fn capture_target(
    control: &ControlPlane,
    focus: Option<&ResourceId>,
    group: Option<[u8; 16]>,
) -> ActionTarget {
    let org = control.organization();
    let topo = control.topology();
    let session = control.selected_session();
    let row = topo.and_then(|topo| topo.sessions.iter().find(|row| Some(row.id) == session));
    let mut target = ActionTarget {
        session_handle: session,
        session_name: row.map(|row| row.name.clone()),
        session: row.and_then(|row| row.key).map(|key| key.to_string()),
        ..ActionTarget::default()
    };
    let focus = focus.filter(|focus| {
        topo.is_some_and(|topo| topo.resources.iter().any(|row| &row.id == *focus))
    });
    target.resource_handle = focus.map(ToString::to_string);
    target.satellite = focus.and_then(ResourceId::host).map(ToString::to_string);
    if !org.is_live() {
        return target;
    }
    target.authority = org.authority().map(|authority| authority.to_string());
    let record = focus.and_then(|focus| org.resource_by_live(focus));
    target.resource = record.map(|record| record.key.to_string());
    if let Some(activation) = record.and_then(|record| record.activation) {
        target.activation = Some(activation.id.get());
        target.instance = Some(hex_id(activation.instance.as_bytes()));
    }
    if let Some(stored) = row
        .and_then(|row| row.arrangement)
        .and_then(|key| org.arrangement(&key))
    {
        target.arrangement = Some(stored.key.to_string());
        if let ArrangementOwner::Workspace(workspace) = stored.owner {
            target.workspace = Some(workspace.to_string());
        }
        if let Some((group, entry)) = placement(stored, group, focus, record) {
            target.group = Some(hex_id(&group));
            target.entry = Some(hex_id(&entry));
        }
    }
    target
}

fn placement(
    stored: &phux_protocol::wire::frame::ArrangementValue,
    group: Option<[u8; 16]>,
    focus: Option<&ResourceId>,
    record: Option<&ResourceRecord>,
) -> Option<([u8; 16], [u8; 16])> {
    let group = stored
        .document
        .groups
        .iter()
        .find(|candidate| Some(candidate.id) == group)?;
    let element = match focus? {
        ResourceId::Local { .. } => EntryTarget::Member(record?.key),
        ResourceId::Satellite { host, id } => EntryTarget::Satellite {
            host: host.clone(),
            id: *id,
        },
    };
    let entry = unique_entry_for_target(group, &element)?;
    Some((group.id, entry))
}

fn hex_id(bytes: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn worker<T: Send + 'static>(
    waker: Option<rune::Waker>,
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<Receiver<Result<T, String>>, String> {
    let (send, receive) = channel();
    std::thread::Builder::new()
        .name("phux-rune-action".into())
        .spawn(move || {
            let _ = send.send(job());
            if let Some(wake) = waker {
                wake();
            }
        })
        .map_err(|error| format!("Couldn't start the plugin action worker: {error}"))?;
    Ok(receive)
}

fn receive<T>(slot: &mut Option<Receiver<Result<T, String>>>) -> Option<Result<T, String>> {
    let result = match slot.as_ref()?.try_recv() {
        Ok(value) => value,
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => Err("The plugin action worker stopped.".into()),
    };
    *slot = None;
    Some(result)
}
